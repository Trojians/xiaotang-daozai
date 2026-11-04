//! 网易云原生播放事件：只读增量日志，不依赖 SMTC 或特定 DLL 版本。
use super::{MediaKind,NowPlaying};
use std::{fs::File,io::{Read,Seek,SeekFrom},path::Path,sync::{mpsc,Arc},time::{Duration,Instant}};

const MAX_READ:u64=4*1024*1024;
const MAX_LINE:usize=256*1024;

// elog 的逐字节替换格式，参考 https://github.com/344272586-sketch/dsh-pet-qqmusic/blob/main/lib/netease-elog.js
fn decode(byte:u8)->u8 {
    let high=byte>>4;let low=byte&15;
    ((high^((low+8)&15))<<4)|((byte>>6)<<2)|(!high&3)
}

fn song_id(value:&serde_json::Value)->Option<u64>{value.as_u64().or_else(||value.as_str()?.parse().ok()).filter(|id|*id>0)}

fn cover_url(value:&str)->Option<String>{
    let mut url=url::Url::parse(value).ok()?;
    let host=url.host_str()?;
    if !matches!(url.scheme(),"http"|"https") || !(host=="music.126.net" || host.ends_with(".music.126.net")){return None;}
    url.set_scheme("https").ok()?;url.set_query(Some("param=256y256"));url.set_fragment(None);
    Some(url.into())
}

fn track(value:&serde_json::Value)->Option<(NowPlaying,String)>{
    let t=value.get("track").unwrap_or(value);
    let id=song_id(t.get("id")?)?;
    let title=t.get("name")?.as_str()?.trim().to_owned();
    let artist=t.get("artists").and_then(|a|a.as_array()).map(|a|a.iter().filter_map(|a|a.get("name")?.as_str()).collect::<Vec<_>>().join("/")).unwrap_or_default();
    if title.is_empty(){return None;}
    let duration=t.get("duration").and_then(|v|v.as_f64()).unwrap_or_default()/1000.0;
    if !duration.is_finite() || !(0.0..86400.0).contains(&duration){return None;}
    let cover=t.pointer("/album/picUrl").and_then(|v|v.as_str()).and_then(cover_url).unwrap_or_default();
    Some((NowPlaying{title,artist,album:t.pointer("/album/name").and_then(|v|v.as_str()).unwrap_or_default().into(),
        source:"cloudmusic.exe".into(),song_id:Some(id),duration,playback_type:MediaKind::Music,..Default::default()},cover))
}

#[derive(Default)]
struct Playback {media:NowPlaying,artwork_url:String,at:u64,ready:bool}
impl Playback {
    fn position(&self,now:u64)->f64{
        let position=self.media.position+if self.media.playing {now.saturating_sub(self.at)as f64/1000.0}else{0.0};
        if self.media.duration>0.0{position.min(self.media.duration)}else{position}
    }
    fn metadata(&mut self,(metadata,cover):(NowPlaying,String)){
        self.media.title=metadata.title;self.media.artist=metadata.artist;
        self.media.album=metadata.album;self.media.duration=metadata.duration;
        self.media.source=metadata.source;self.media.playback_type=metadata.playback_type;
        if !cover.is_empty(){self.artwork_url=cover;}
    }
    fn line(&mut self,line:&str,pid:u32,now:u64){
        let Some((header,body))=line.strip_prefix('[').and_then(|s|s.split_once(']'))else{return;};
        let mut parts=header.split(':');
        if parts.next().and_then(|s|s.parse::<u32>().ok())!=Some(pid){return;}
        let Some(at)=parts.nth(2).and_then(|s|s.parse::<u64>().ok()).filter(|at|*at<=now.saturating_add(1000) && *at>=self.at)else{return;};
        if body.contains("\"exitApp\""){*self=Self::default();return;}
        // 只解析播放事件；音频签名链接、Cookie 等其他日志内容不会进入状态或缓存。
        let Some((_,body))=body.split_once("【playing】,")else{return;};
        let Some((event,args))=body.trim().strip_prefix('"').and_then(|s|s.split_once("\","))else{return;};
        if matches!(event,"playOneTrackInPlayingList"|"checkPlayPrivilege"){
            let Some(metadata)=serde_json::from_str(args).ok().as_ref().and_then(track)else{return;};
            if self.media.song_id!=metadata.0.song_id {
                if event=="checkPlayPrivilege" && self.media.song_id.is_some(){return;}
                *self=Self{media:metadata.0,artwork_url:metadata.1,at,ready:false};
            }else{self.metadata(metadata);}
        }else if event.starts_with("native") && event.contains("load完成"){
            let Some(id)=serde_json::from_str::<serde_json::Value>(args).ok().and_then(|v|song_id(v.get("songId")?))else{return;};
            if self.media.song_id!=Some(id){*self=Self::default();self.media.song_id=Some(id);}
            self.media.position=0.0;self.media.playing=true;self.at=at;self.ready=true;
        }else if event=="setPlayingPosition" && self.media.song_id.is_some(){
            let Some(position)=args.split(',').next().and_then(|s|s.trim().parse::<f64>().ok()).filter(|p|p.is_finite() && (0.0..86400.0).contains(p))else{return;};
            self.media.position=position;self.at=at;self.ready=true;
        }else if event.starts_with("native") && event.ends_with("state") && self.media.song_id.is_some(){
            let Some(status)=args.split(',').next().and_then(|s|s.trim().parse::<u8>().ok()).filter(|s|matches!(s,1|2))else{return;};
            self.media.position=self.position(at);self.media.playing=status==1;self.at=at;self.ready=true;
        }
    }
    fn snapshot(&self,now:u64)->Option<NowPlaying>{
        if !self.ready || !self.media.is_music(){return None;}
        let mut media=self.media.clone();media.position=self.position(now);media.sampled_at=Some(std::time::Instant::now());
        media.event_time_ms=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()
            .map(|utc|(utc.as_millis() as u64).saturating_sub(now.saturating_sub(self.at)));Some(media)
    }
}

#[derive(Default)]
struct CoverCache {
    url:String,image:Option<Arc<eframe::egui::ColorImage>>,
    pending:Option<mpsc::Receiver<Option<Arc<eframe::egui::ColorImage>>>>,
    retry_at:Option<Instant>,
}
impl CoverCache {
    fn get(&mut self,url:&str)->Option<Arc<eframe::egui::ColorImage>>{
        // 切歌时丢弃旧请求的接收端，迟到的封面不能覆盖新歌。
        if self.url!=url{*self=Self{url:url.into(),..Default::default()};}
        if self.url.is_empty(){return None;}
        if let Some(receiver)=&self.pending{
            match receiver.try_recv(){
                Ok(image)=>{self.image=image;self.pending=None;}
                Err(mpsc::TryRecvError::Disconnected)=>{self.pending=None;}
                Err(mpsc::TryRecvError::Empty)=>{}
            }
        }
        if self.image.is_none() && self.pending.is_none() && self.retry_at.is_none_or(|at|Instant::now()>=at){
            let (sender,receiver)=mpsc::channel();let url=self.url.clone();
            self.pending=Some(receiver);self.retry_at=Some(Instant::now()+Duration::from_secs(3));
            // 网络与解码不占用媒体轮询线程，歌词时间轴继续正常推进。
            std::thread::spawn(move||{let _=sender.send(fetch_cover(&url));});
        }
        self.image.clone()
    }
}

#[test]
fn cover_failure_can_retry_without_a_minute_of_blank_artwork(){
    let mut cache=CoverCache::default();
    let before=Instant::now();
    cache.get("invalid-test-url");
    assert!(cache.retry_at.unwrap().duration_since(before)<Duration::from_secs(5),"封面暂时失败后不能空白一分钟");
}

fn fetch_cover(url:&str)->Option<Arc<eframe::egui::ColorImage>>{
    let url=cover_url(url)?;
    let response=ureq::AgentBuilder::new().timeout(Duration::from_secs(5)).redirects(0).build().get(&url).call().ok()?;
    let limit=2*1024*1024;let mut bytes=Vec::new();
    response.into_reader().take(limit+1).read_to_end(&mut bytes).ok()?;
    if bytes.len()>limit as usize{return None;}
    super::decode_artwork(&bytes).map(Arc::new)
}

#[derive(Default)]
pub(super) struct Reader {
    pid:u32,offset:u64,prefix:Vec<u8>,carry:Vec<u8>,skip_line:bool,
    playback:Playback,lookup:(Option<u64>,u64),cover:CoverCache,
}
impl Reader {
    pub(super) fn pending(&self)->bool {
        self.pid!=0 && self.playback.media.song_id.is_some() && (!self.playback.ready || !self.playback.media.is_music())
    }
    fn ingest(&mut self,bytes:&[u8],now:u64){
        for byte in bytes.iter().copied().map(decode){
            if byte==b'\n'{
                if !self.skip_line{
                    if let Ok(line)=std::str::from_utf8(&self.carry){self.playback.line(line,self.pid,now);}
                }
                self.carry.clear();self.skip_line=false;
            }else if !self.skip_line{
                if self.carry.len()<MAX_LINE{self.carry.push(byte);}else{self.carry.clear();self.skip_line=true;}
            }
        }
    }
    fn read(&mut self,path:&Path,pid:Option<u32>,now:u64)->Option<NowPlaying>{
        let Some(pid)=pid else{*self=Self::default();return None;};
        if self.pid!=pid{*self=Self{pid,..Self::default()};}
        let mut file=match File::open(path){Ok(file)=>file,Err(_)=>{*self=Self{pid,..Self::default()};return None;}};
        let size=file.metadata().ok()?.len();
        let mut prefix=vec![0;64.min(size as usize)];file.read_exact(&mut prefix).ok()?;
        if size<self.offset || !prefix.starts_with(&self.prefix){*self=Self{pid,..Self::default()};}
        self.prefix=prefix;
        if size>self.offset{
            let start=self.offset.max(size.saturating_sub(MAX_READ));
            if start>self.offset{self.carry.clear();self.skip_line=true;}
            file.seek(SeekFrom::Start(start)).ok()?;
            let mut bytes=vec![0;(size-start)as usize];file.read_exact(&mut bytes).ok()?;
            self.offset=size;self.ingest(&bytes,now);
        }
        // 启动时续播可能只写歌曲 ID，按该 ID 补充本地队列里的元数据。
        let id=self.playback.media.song_id;
        if id.is_some() && (!self.playback.media.is_music() || self.playback.artwork_url.is_empty()) && (self.lookup.0!=id || now.saturating_sub(self.lookup.1)>=5000){
            self.lookup=(id,now);
            if let Some(metadata)=queue_track(path.parent()?,id?){self.playback.metadata(metadata);}
        }
        self.playback.snapshot(now)
    }
    pub(super) fn poll(&mut self)->Option<NowPlaying>{
        let path=std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("NetEase/CloudMusic/cloudmusic.elog");
        let sampled=std::time::Instant::now();
        let mut media=self.read(&path,process_id(),unsafe{windows::Win32::System::SystemInformation::GetTickCount64()})?;
        if self.cover.url==self.playback.artwork_url{media.artwork=self.cover.image.clone();}
        media.sampled_at=Some(sampled);Some(media)
    }
    pub(super) fn fill_artwork(&mut self,media:&mut NowPlaying){
        if media.source.eq_ignore_ascii_case("cloudmusic.exe") && media.song_id==self.playback.media.song_id && media.artwork.is_none(){
            media.artwork=self.cover.get(&self.playback.artwork_url);
        }
    }
}

fn queue_track(root:&Path,id:u64)->Option<(NowPlaying,String)>{
    for (file,key) in [("playingList","list"),("fmPlay","queue")]{
        let Some(value)=(||{
            let file=File::open(root.join("webdata/file").join(file)).ok()?;
            if file.metadata().ok()?.len()>8*1024*1024{return None;}
            serde_json::from_reader::<_,serde_json::Value>(file).ok()
        })()else{continue;};
        if let Some(found)=value.get(key).and_then(|v|v.as_array()).and_then(|list|list.iter().filter_map(track).find(|t|t.0.song_id==Some(id))){return Some(found);}
    }
    None
}

fn process_id()->Option<u32>{
    use windows::{core::{w,PCWSTR},Win32::UI::WindowsAndMessaging::{FindWindowW,GetWindowThreadProcessId}};
    unsafe{let window=FindWindowW(w!("OrpheusBrowserHost"),PCWSTR::null()).ok()?;let mut pid=0;GetWindowThreadProcessId(window,Some(&mut pid));(pid!=0).then_some(pid)}
}

fn control_hotkey(command:super::Transport)->Option<&'static str>{
    match command {super::Transport::Toggle=>Some("play_pause_local"),super::Transport::Next=>Some("next_local"),
        super::Transport::Prev=>Some("prev_local"),super::Transport::Seek(_)=>None}
}

pub(super) fn control(command:super::Transport){
    use windows::{core::{w,HSTRING,PCWSTR},Win32::{Foundation::{LPARAM,WPARAM},System::DataExchange::GlobalFindAtomW,
        UI::WindowsAndMessaging::{FindWindowW,PostMessageW,WM_HOTKEY}}};
    let Some(name)=control_hotkey(command) else{return;};
    unsafe{if let Ok(window)=FindWindowW(w!("OrpheusBrowserHost"),PCWSTR::null()){
        // 读取网易云当前注册的命令，不缓存随进程变化的编号，也不广播系统媒体键。
        let atom=GlobalFindAtomW(&HSTRING::from(name));
        if atom!=0 {let _=PostMessageW(window,WM_HOTKEY,WPARAM(atom as usize),LPARAM(0));}
    }}
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn incomplete_artist_does_not_discard_a_known_native_song(){
        let (media,_)=track(&serde_json::json!({"id":123,"name":"待补全的歌曲","duration":120000})).unwrap();
        assert!(media.artist.is_empty() && media.is_music());assert_eq!(media.song_id,Some(123));
    }
    #[test]
    fn transport_uses_only_netease_local_commands(){
        use super::super::Transport;
        assert_eq!(control_hotkey(Transport::Toggle),Some("play_pause_local"));
        assert_eq!(control_hotkey(Transport::Next),Some("next_local"));
        assert_eq!(control_hotkey(Transport::Prev),Some("prev_local"));
        assert_eq!(control_hotkey(Transport::Seek(30.0)),None);
    }
    #[test]
    fn native_track_reads_public_album_cover_and_clears_it_on_song_change(){
        let mut state=Playback::default();
        let data=serde_json::json!({"id":123,"name":"歌曲","artists":[{"name":"歌手"}],"album":{"name":"专辑","picUrl":"http://p3.music.126.net/cover.jpg"}});
        state.line(&event(7,1000,&format!("\"playOneTrackInPlayingList\",{data}")),7,10000);
        assert_eq!(state.artwork_url,"https://p3.music.126.net/cover.jpg?param=256y256");
        state.line(&event(7,2000,"\"native播放资源load完成\",{\"songId\":\"456\"}"),7,10000);
        assert!(state.artwork_url.is_empty());
        for url in ["file:///cover.jpg","https://music.126.net.evil.test/cover.jpg","https://example.com/cover.jpg"]{assert!(cover_url(url).is_none());}
    }
    #[test]
    fn native_cover_cache_reuses_image_and_discards_late_results(){
        let image=Arc::new(eframe::egui::ColorImage::new([2,2],eframe::egui::Color32::BLUE));
        let (tx,rx)=mpsc::channel();
        let mut cache=CoverCache{url:"cover-a".into(),pending:Some(rx),..Default::default()};
        assert!(cache.get("cover-a").is_none(),"封面下载中不能阻塞媒体刷新");
        tx.send(Some(image.clone())).unwrap();
        assert!(Arc::ptr_eq(&cache.get("cover-a").unwrap(),&image));
        assert!(Arc::ptr_eq(&cache.get("cover-a").unwrap(),&image));
        let (tx,rx)=mpsc::channel();cache.pending=Some(rx);
        assert!(cache.get("").is_none());assert!(tx.send(Some(image)).is_err(),"旧请求不得把封面写入新歌");
    }
    #[test]
    fn native_cover_only_fills_the_selected_netease_song(){
        let image=Arc::new(eframe::egui::ColorImage::new([2,2],eframe::egui::Color32::BLUE));
        let mut reader=Reader::default();reader.playback.media.song_id=Some(123);reader.playback.artwork_url="cover".into();
        reader.cover=CoverCache{url:"cover".into(),image:Some(image.clone()),..Default::default()};
        for source in ["SodaMusic.exe","KuGou.exe","QQMusic.exe","Spotify.exe"]{
            let mut media=NowPlaying{source:source.into(),song_id:Some(123),..Default::default()};reader.fill_artwork(&mut media);assert!(media.artwork.is_none());
        }
        let mut media=NowPlaying{source:"cloudmusic.exe".into(),song_id:Some(456),..Default::default()};reader.fill_artwork(&mut media);assert!(media.artwork.is_none());
        media.song_id=Some(123);reader.fill_artwork(&mut media);assert!(Arc::ptr_eq(media.artwork.as_ref().unwrap(),&image));
        let system_image=Arc::new(eframe::egui::ColorImage::new([2,2],eframe::egui::Color32::RED));
        media.artwork=Some(system_image.clone());reader.fill_artwork(&mut media);assert!(Arc::ptr_eq(media.artwork.as_ref().unwrap(),&system_image));
    }
    fn event(pid:u32,at:u64,body:&str)->String{format!("[{pid}:1:1103/170000:{at}:INFO:app.cpp(1)] 【playing】,{body}\n")}
    fn metadata()->String{format!("\"playOneTrackInPlayingList\",{}",serde_json::json!({"track":{"id":"123","name":"测试歌曲","artists":[{"name":"歌手"}],"duration":240000,"album":{"name":"专辑"}}}))}
    fn encode(s:&str)->Vec<u8>{let mut table=[0;256];for b in 0..=255{table[decode(b)as usize]=b;}s.bytes().map(|b|table[b as usize]).collect()}
    #[test]
    fn native_log_tracks_load_pause_resume_seek_and_ignores_other_processes(){
        let mut state=Playback::default();
        state.line(&event(7,1000,&metadata()),7,10000);assert!(state.snapshot(1500).is_none());
        state.line(&event(7,2000,"\"native播放资源load完成，开始播放\",{\"songId\":\"123\"}"),7,10000);
        assert_eq!(state.snapshot(3000).unwrap().position,1.0);
        state.line(&event(8,3500,"\"setPlayingPosition\",99"),7,10000);
        state.line(&event(7,4000,"\"native播放state\",2"),7,10000);
        assert_eq!(state.snapshot(8000).unwrap().position,2.0);
        state.line(&event(7,5000,"\"setPlayingPosition\",30.5"),7,10000);
        state.line(&event(7,6000,"\"native播放state\",1"),7,10000);
        let media=state.snapshot(7000).unwrap();assert!(media.playing);assert_eq!(media.position,31.5);assert_eq!(media.song_id,Some(123));
        state.line(&event(7,8000,"\"setPlayingPosition\",NaN"),7,10000);assert_eq!(state.snapshot(8000).unwrap().position,32.5);
        state.line(&event(7,9000,"\"exitApp\""),7,10000);assert!(state.snapshot(10000).is_none());
    }
    #[test]
    fn native_log_handles_partial_utf8_and_new_process_without_stale_tracks(){
        let dir=std::env::temp_dir().join(format!("daozai-elog-{}",std::process::id()));std::fs::create_dir_all(&dir).unwrap();let path=dir.join("cloudmusic.elog");
        let raw=encode(&(event(7,1000,&metadata())+&event(7,2000,"\"native播放资源load完成\",{\"songId\":\"123\"}")));
        let mut reader=Reader{pid:7,..Default::default()};
        for byte in &raw{reader.ingest(&[*byte],3000);}
        assert_eq!(reader.playback.snapshot(3000).unwrap().title,"测试歌曲");
        std::fs::write(&path,&raw).unwrap();reader=Reader::default();
        assert_eq!(reader.read(&path,Some(7),3000).unwrap().position,1.0);
        assert_eq!(reader.read(&path,Some(7),4000).unwrap().position,2.0);
        assert!(reader.read(&path,None,4000).is_none());
        assert!(reader.read(&path,Some(8),4000).is_none(),"新进程不能继承旧进程的歌曲");
        let raw=encode(&(event(8,5000,&metadata())+&event(8,6000,"\"native播放资源load完成\",{\"songId\":\"123\"}")));
        std::fs::write(&path,&raw).unwrap();assert_eq!(reader.read(&path,Some(8),7000).unwrap().position,1.0);
        std::fs::write(&path,b"").unwrap();assert!(reader.read(&path,Some(8),7000).is_none());
        std::fs::remove_file(path).unwrap();std::fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn native_log_resumes_from_queue_metadata_without_any_smtc_session(){
        let dir=std::env::temp_dir().join(format!("daozai-elog-queue-{}",std::process::id()));let queue=dir.join("webdata/file");std::fs::create_dir_all(&queue).unwrap();
        let data=serde_json::json!({"list":[{"track":{"id":123,"name":"续播歌曲","artists":[{"name":"歌手"}],"duration":240000,"album":{"picUrl":"https://p1.music.126.net/resume.jpg"}}}]});
        std::fs::write(queue.join("playingList"),data.to_string()).unwrap();let path=dir.join("cloudmusic.elog");
        std::fs::write(&path,encode(&(event(9,1000,"\"native播放资源load完成\",{\"songId\":\"123\"}")+&event(9,1100,"\"setPlayingPosition\",80")))).unwrap();
        let mut reader=Reader::default();let media=reader.read(&path,Some(9),2100).unwrap();assert_eq!(media.title,"续播歌曲");assert_eq!(media.position,81.0);
        assert_eq!(reader.playback.artwork_url,"https://p1.music.126.net/resume.jpg?param=256y256");
        std::fs::remove_file(path).unwrap();std::fs::remove_file(queue.join("playingList")).unwrap();std::fs::remove_dir(&queue).unwrap();std::fs::remove_dir(dir.join("webdata")).unwrap();std::fs::remove_dir(dir).unwrap();
    }
    #[test]
    #[ignore="需要本机网易云正在播放，用于验证无 SMTC 的原生数据和歌词"]
    fn live_native_netease_playback_and_lyrics(){
        let mut reader=Reader::default();let first=reader.poll().expect("未读到网易云原生播放事件");
        println!("歌曲={} 歌手={} ID={:?} 播放={} 进度={:.2}/{:.2}",first.title,first.artist,first.song_id,first.playing,first.position,first.duration);
        let lyrics=crate::lyrics::fetch_for_source(&first.source,&first.title,&first.artist,first.duration,first.song_id).unwrap();
        assert!(lyrics.synced && !lyrics.lines.is_empty());println!("已获取 {} 行同步歌词",lyrics.lines.len());
        std::thread::sleep(std::time::Duration::from_secs(2));let next=reader.poll().unwrap();
        if first.playing && next.playing && first.song_id==next.song_id{assert!(next.position>first.position);}
    }
    #[test]
    #[ignore="需要本机网易云正在播放，用于验证原生封面与缓存"]
    fn live_native_netease_album_cover(){
        let mut reader=Reader::default();let start=Instant::now();
        loop{
            let mut media=reader.poll().expect("未读到网易云原生播放事件");reader.fill_artwork(&mut media);
            if let Some(image)=&media.artwork{
                assert!(image.size[0]>1 && image.size[0]<=256 && image.size[1]>1 && image.size[1]<=256);
                let mut next=reader.poll().unwrap();reader.fill_artwork(&mut next);
                if next.song_id==media.song_id{assert!(Arc::ptr_eq(next.artwork.as_ref().unwrap(),image));}
                println!("歌曲={} 封面={}x{}，重复采样复用缓存",media.title,image.size[0],image.size[1]);break;
            }
            assert!(start.elapsed()<Duration::from_secs(10),"未获取到网易云专辑封面");std::thread::sleep(Duration::from_millis(100));
        }
    }
}
