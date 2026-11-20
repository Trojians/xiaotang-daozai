//! 推送订阅（PushMe 协议，轮询版）。
//!
//! 后台线程每隔 N 秒 `GET {server}/api/pull?push_key=...&after_id=...`，
//! 新消息发事件给主线程；主线程弹岛上提醒卡片 + 托盘通知。
//! 不引 tokio、MQTT、Markdown 渲染库；凭据只进 Windows 凭据管理器（`keyring`，
//! 和邮件同一套库）；历史存在 `%APPDATA%\Daozai\push.json`。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

/// 凭据管理器里的服务名。
pub const SERVICE: &str = "Daozai.Push";

/// 一条推送消息。
#[derive(Clone, Debug)]
pub struct Message {
    /// 服务端自增 id，轮询去重与断点续拉都靠它。
    pub id: u64,
    pub title: String,
    pub content: String,
    /// 消息类型：`text` / `markdown` 等；第一版只按纯文本显示。
    pub msg_type: String,
    /// 服务端时间，`YYYY-mm-dd HH:ii:ss`。
    pub time: String,
}

impl Message {
    /// 从服务端拉取接口返回的 JSON 对象解析；缺字段时给默认值，id 缺失则丢弃。
    fn from_value(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            id: v.get("id")?.as_u64()?,
            title: v.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            content: v.get("content").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            msg_type: v.get("type").and_then(|t| t.as_str()).unwrap_or("text").to_string(),
            time: v.get("time").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        })
    }

    fn to_value(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "title": self.title,
            "content": self.content,
            "type": self.msg_type,
            "time": self.time,
        })
    }
}

/// 工作线程发给主线程的事件。
pub enum Event {
    /// 收到一条新消息。
    Message { msg: Message },
    /// 拉取失败（网络、鉴权、服务端错误等）；主线程连续收到两次才显示。
    Error(String),
}

enum Cmd {
    Sync { server: String, interval: u32 },
    Stop,
}

/// 推送轮询服务：一个管理线程 + 一个工作线程，与 `mail::Service` 同款结构。
pub struct Service {
    cmd: Sender<Cmd>,
    ev: Receiver<Event>,
}

impl Service {
    pub fn start() -> Self {
        let (ev_tx, ev_rx) = channel();
        let (cmd_tx, cmd_rx) = channel();
        let _ = std::thread::Builder::new()
            .name("daozai-push".into())
            .spawn(move || supervisor(cmd_rx, ev_tx));
        Self { cmd: cmd_tx, ev: ev_rx }
    }

    /// 开始（或按新参数重启）轮询。key 由工作线程自己从凭据管理器读。
    pub fn sync(&self, server: &str, interval_secs: u32) {
        let _ = self.cmd.send(Cmd::Sync {
            server: server.to_string(),
            interval: interval_secs,
        });
    }

    /// 停止轮询线程。
    pub fn stop(&self) {
        let _ = self.cmd.send(Cmd::Stop);
    }

    pub fn try_recv(&self) -> Option<Event> {
        self.ev.try_recv().ok()
    }
}

fn supervisor(cmd_rx: Receiver<Cmd>, ev: Sender<Event>) {
    let mut running: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)> = None;
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Cmd::Sync { server, interval } => {
                if let Some((flag, _)) = running.take() {
                    flag.store(true, Ordering::Relaxed);
                }
                let stop = Arc::new(AtomicBool::new(false));
                let flag = stop.clone();
                let ev = ev.clone();
                if let Ok(handle) = std::thread::Builder::new()
                    .name("daozai-push-worker".into())
                    .spawn(move || worker(server, interval, flag, ev))
                {
                    running = Some((stop, handle));
                }
            }
            Cmd::Stop => {
                if let Some((flag, _)) = running.take() {
                    flag.store(true, Ordering::Relaxed);
                }
            }
        }
    }
    if let Some((flag, _)) = running.take() {
        flag.store(true, Ordering::Relaxed);
    }
}

/// 可中断的睡眠；返回 true 表示收到停止信号。
fn sleep_stop(stop: &AtomicBool, secs: u64) -> bool {
    for _ in 0..secs * 2 {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// 工作线程：立即拉一次，之后按间隔轮询；key 每轮都重读，设置页里刚保存就能用上。
fn worker(server: String, interval: u32, stop: Arc<AtomicBool>, ev: Sender<Event>) {
    let mut after_id = history::load_last_id();
    loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match secrets::load() {
            Ok(key) => match fetch(&server, &key, after_id) {
                Ok((msgs, last_id)) => {
                    if last_id > after_id {
                        after_id = last_id;
                        history::save_last_id(after_id);
                    }
                    for m in msgs {
                        history::append(&m);
                        let _ = ev.send(Event::Message { msg: m });
                    }
                }
                Err(e) => {
                    let _ = ev.send(Event::Error(e));
                }
            },
            Err(e) => {
                let _ = ev.send(Event::Error(e));
            }
        }
        if sleep_stop(&stop, interval.clamp(30, 900) as u64) {
            return;
        }
    }
}

/// 拉取一次：返回（新消息，服务端当前最大 id）。
fn fetch(server: &str, key: &str, after_id: u64) -> Result<(Vec<Message>, u64), String> {
    let base = server.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("服务器地址为空".into());
    }
    let url = format!(
        "{}/api/pull?push_key={}&after_id={}",
        base,
        percent_encode(key),
        after_id
    );
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .build();
    let resp = agent.get(&url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("服务器返回 {code}"),
        _ => format!("连接失败：{e}"),
    })?;
    let text = resp
        .into_string()
        .map_err(|e| format!("读取响应失败：{e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("响应解析失败：{e}"))?;
    if v.get("code").and_then(|c| c.as_u64()) != Some(0) {
        let errmsg = v
            .get("errmsg")
            .and_then(|m| m.as_str())
            .unwrap_or("服务器拒绝");
        return Err(errmsg.to_string());
    }
    let messages = v
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|arr| arr.iter().filter_map(Message::from_value).collect())
        .unwrap_or_default();
    let last_id = v
        .get("last_id")
        .and_then(|n| n.as_u64())
        .unwrap_or(after_id);
    Ok((messages, last_id))
}

/// 只编码 query 参数值里不安全的字节；字母数字与 `-_.~` 原样保留。
fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------- 凭据

#[cfg(windows)]
pub mod secrets {
    /// 凭据只进 Windows 凭据管理器；服务名 `Daozai.Push`，账号固定为 `push_key`。
    pub fn save(key: &str) -> Result<(), String> {
        let entry = keyring::Entry::new(super::SERVICE, "push_key")
            .map_err(|e| format!("凭据管理器不可用：{e}"))?;
        entry
            .set_password(key)
            .map_err(|e| format!("保存 push_key 失败：{e}"))
    }

    pub fn load() -> Result<String, String> {
        let entry = keyring::Entry::new(super::SERVICE, "push_key")
            .map_err(|e| format!("凭据管理器不可用：{e}"))?;
        entry.get_password().map_err(|_| "没有保存 push_key".to_string())
    }

    pub fn delete() {
        if let Ok(entry) = keyring::Entry::new(super::SERVICE, "push_key") {
            let _ = entry.delete_credential();
        }
    }
}

#[cfg(not(windows))]
pub mod secrets {
    pub fn save(_key: &str) -> Result<(), String> {
        Err("仅 Windows 支持凭据管理器".into())
    }

    pub fn load() -> Result<String, String> {
        Err("没有保存 push_key".to_string())
    }

    pub fn delete() {}
}

// ---------------------------------------------------------------- 历史

/// 本地历史：`%APPDATA%\Daozai\push.json`，`{last_id, messages[]}`，最多留 100 条。
pub mod history {
    use super::Message;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        crate::model::config_dir().join("push.json")
    }

    /// 读整份历史；文件不存在或损坏时返回空。
    fn load() -> (u64, Vec<Message>) {
        let Ok(text) = std::fs::read_to_string(path()) else {
            return (0, Vec::new());
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return (0, Vec::new());
        };
        let last_id = v.get("last_id").and_then(|n| n.as_u64()).unwrap_or(0);
        let messages = v
            .get("messages")
            .and_then(|m| m.as_array())
            .map(|arr| arr.iter().filter_map(Message::from_value).collect())
            .unwrap_or_default();
        (last_id, messages)
    }

    fn save(last_id: u64, messages: &[Message]) {
        let body = serde_json::json!({
            "last_id": last_id,
            "messages": messages.iter().map(Message::to_value).collect::<Vec<_>>(),
        });
        let text = serde_json::to_string_pretty(&body).unwrap_or_default();
        let _ = crate::model::atomic_write(&path(), text.as_bytes());
    }

    /// 启动时恢复：上次拉到的最大 id，避免重启后把旧消息再提醒一遍。
    pub fn load_last_id() -> u64 {
        load().0
    }

    /// 启动时恢复：最近消息（新的在前）。
    pub fn load_messages() -> Vec<Message> {
        load().1
    }

    /// 只更新断点（本轮没有新消息时也记下来）。
    pub fn save_last_id(last_id: u64) {
        let (_, messages) = load();
        save(last_id, &messages);
    }

    /// 存一条新消息（去重后插到最前，最多 100 条）。
    pub fn append(msg: &Message) {
        let (last_id, mut messages) = load();
        messages.retain(|m| m.id != msg.id);
        messages.insert(0, msg.clone());
        messages.truncate(100);
        save(last_id.max(msg.id), &messages);
    }

    /// 清空历史（断点与消息一起清掉）。
    pub fn clear() {
        save(0, &[]);
    }
}
