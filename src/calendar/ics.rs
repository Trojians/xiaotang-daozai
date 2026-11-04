//! ICS 订阅：直接拉取公开发布的 .ics 文件（例如 Outlook 网页版「发布日历」得到的链接），只读。
//!
//! 链接本身就是凭据：调用方把它放在 Windows 凭据管理器里当「密码」传进来，
//! 本模块不落盘、不记日志、不在任何错误里回显链接。
//! 解析与重复展开复用 `mail::meetings_of` 与 `super::events_from_ics`，
//! 与 CalDAV / Exchange ActiveSync 走同一套日程结构。

use super::{agent, events_from_ics, Failure, Fetched, Provider};
use chrono::{DateTime, Utc};

/// 拉取订阅文件，返回窗口内的日程。`url` 就是订阅链接本身。
pub fn fetch(url: &str, lo: DateTime<Utc>, hi: DateTime<Utc>) -> Result<Fetched, Failure> {
    let mut target = validate(url)?;
    for _ in 0..4 {
        let resp = agent().get(target.as_str()).call().map_err(|e| match e {
            ureq::Error::Status(code, _) => Failure::status(Provider::Ics, code),
            _ => Failure::transport(""),
        })?;
        let status = resp.status();
        if (301..=308).contains(&status) {
            let loc = resp
                .header("Location")
                .ok_or_else(|| Failure::text("订阅链接重定向缺少地址"))?;
            target = target
                .join(loc)
                .map_err(|_| Failure::text("订阅链接重定向地址无效"))?;
            // 每跳都重验：只接受 https（本地测试可用 http 回环地址），且不带账号密码。
            // 不要求同源：发布链接常在微软域名之间跳转；请求里不带任何凭据头，
            // 泄露面只有链接本身，而链接的发布方本就是公开端点。
            target = validate(target.as_str())?;
            continue;
        }
        if status >= 300 {
            return Err(Failure::status(Provider::Ics, status));
        }
        let text = resp.into_string().map_err(|_| Failure::transport(""))?;
        let events = events_from_ics(&text, lo, hi)?;
        return Ok(Fetched {
            href: url.to_string(),
            events,
        });
    }
    Err(Failure::text("订阅链接重定向次数过多"))
}

/// 只接受 https 链接（本地测试可用 http 回环地址），且链接里不能带账号密码。
fn validate(url: &str) -> Result<url::Url, Failure> {
    let target = url::Url::parse(url).map_err(|_| Failure::text("订阅链接无效"))?;
    let loopback = target
        .host_str()
        .is_some_and(|host| host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()));
    if !target.username().is_empty()
        || target.password().is_some()
        || !(target.scheme() == "https" || target.scheme() == "http" && loopback)
    {
        return Err(Failure::text(
            "订阅链接无效：只接受 https 链接（本地测试可用 http 回环地址），且链接里不能带账号密码",
        ));
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// 起一个只回一次固定响应的本地 HTTP 服务，返回它的订阅链接。
    fn serve_once(body: &str, status: &str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_string();
        let status = status.to_string();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: text/calendar\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://{addr}/cal.ics")
    }

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        chrono::Utc
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .unwrap()
    }

    const SAMPLE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//Test//CN\r\n\
        BEGIN:VEVENT\r\nUID:standup\r\nSUMMARY:站会\r\nDTSTART:20261105T020000Z\r\nDTEND:20261105T023000Z\r\n\
        RRULE:FREQ=DAILY;COUNT=3\r\nEXDATE:20261106T020000Z\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:standup\r\nRECURRENCE-ID:20261107T020000Z\r\nSUMMARY:站会（改期）\r\n\
        DTSTART:20261107T030000Z\r\nDTEND:20261107T033000Z\r\nEND:VEVENT\r\n\
        BEGIN:VEVENT\r\nUID:allday\r\nSUMMARY:全天假\r\nDTSTART;VALUE=DATE:20261105\r\nDTEND;VALUE=DATE:20261106\r\nEND:VEVENT\r\n\
        END:VCALENDAR\r\n";

    #[test]
    fn ics_expands_recurrence_applies_override_and_keeps_all_day() {
        let url = serve_once(SAMPLE, "200 OK");
        let got = fetch(&url, utc(2026, 11, 5, 0, 0), utc(2026, 11, 8, 0, 0)).unwrap();
        // 站会：11-05 一次，11-06 被 EXDATE 排除，11-07 被改期实例覆盖；另加全天假一件。
        assert_eq!(got.events.len(), 3);
        let standup: Vec<_> = got
            .events
            .iter()
            .filter(|m| m.uid == "standup")
            .collect();
        assert_eq!(standup.len(), 2);
        assert!(standup.iter().any(|m| m.start == utc(2026, 11, 5, 2, 0).timestamp()));
        let moved = standup
            .iter()
            .find(|m| m.start == utc(2026, 11, 7, 3, 0).timestamp())
            .expect("改期实例应在场");
        assert_eq!(moved.title, "站会（改期）");
        assert!(got.events.iter().any(|m| m.uid == "allday" && m.all_day));
        assert_eq!(got.href, url);
    }

    #[test]
    fn ics_http_error_maps_to_status() {
        let url = serve_once("", "404 Not Found");
        let err = fetch(&url, utc(2026, 11, 5, 0, 0), utc(2026, 11, 6, 0, 0)).unwrap_err();
        assert_eq!(err.status, Some(404));
    }

    #[test]
    fn ics_url_validation_rejects_unsafe_links() {
        assert!(validate("https://outlook.live.com/owa/calendar/abc/reachcalendar.ics").is_ok());
        assert!(validate("http://127.0.0.1:9/cal.ics").is_ok());
        assert!(validate("http://example.com/cal.ics").is_err());
        assert!(validate("https://user:pass@example.com/cal.ics").is_err());
        assert!(validate("not a url").is_err());
    }
}
