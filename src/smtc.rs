//! 系统媒体会话（SMTC）读取：Windows 10/11 的“正在播放”。
//! 与原工具使用同一套 API（GlobalSystemMediaTransportControlsSessionManager）。

#[cfg(windows)]
#[path="netease_timeline.rs"]
mod netease_timeline;
#[cfg(windows)]
#[path="netease_playback.rs"]
mod netease_playback;
#[cfg(windows)]
#[path="kugou_metadata.rs"]
mod kugou_metadata;
#[cfg(windows)]
#[path="kugou_control.rs"]
mod kugou_control;
#[cfg(windows)]
#[path="player_seek.rs"]
mod player_seek;

use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaKind {
    Music,
    Video,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub source: String,
    pub song_id: Option<u64>,
    pub playing: bool,
    pub position: f64,
    /// 播放位置的采样时刻，避免封面读取和线程交接消耗歌词提前量。
    pub sampled_at: Option<std::time::Instant>,
    /// 播放事件的 UTC 毫秒，用于比较系统会话与本机日志的新旧。
    pub event_time_ms: Option<u64>,
    pub duration: f64,
    pub artwork: Option<Arc<eframe::egui::ColorImage>>,
    pub playback_type: MediaKind,
}

impl NowPlaying {
    pub fn kind(&self)->MediaKind {
        if self.title.trim().is_empty(){return MediaKind::Unknown;}
        if self.playback_type==MediaKind::Video {return MediaKind::Video;}
        // 部分浏览器把所有媒体都上报为音乐，补充识别明确的视频页面标题。
        let title=self.title.trim().to_lowercase();
        if [" - 抖音"," - youtube","_哔哩哔哩_bilibili"," - 哔哩哔哩"].iter().any(|suffix|title.ends_with(suffix)) {
            return MediaKind::Video;
        }
        let source=self.source.to_lowercase();
        if matches!(source.as_str(),"kugou"|"kugou.exe"|"qqmusic"|"qqmusic.exe"|"cloudmusic.exe"|"汽水音乐"|"sodamusic.exe"|"spotify"|"spotify.exe"|"kwmusic.exe") && self.playback_type==MediaKind::Music{return MediaKind::Music;}
        if ["chrome","msedge","firefox","brave","opera"].iter().any(|name|source.contains(name)) {
            let album=self.album.trim().to_lowercase();
            if ["youtube","bilibili","抖音","哔哩哔哩"].contains(&album.as_str()){return MediaKind::Video;}
            // 视频作者也可能被放进 Artist；浏览器缺少专辑信息时不自动认作歌曲。
            if album.is_empty(){return MediaKind::Unknown;}
        }
        if self.artist.trim().is_empty(){MediaKind::Unknown}else{MediaKind::Music}
    }

    pub fn is_music(&self)->bool {self.kind()==MediaKind::Music}
}

type ArtworkRequest=(String,Box<dyn FnOnce()->Option<Vec<u8>>+Send>);
struct ArtworkWorker {
    requests:std::sync::mpsc::Sender<ArtworkRequest>,
    results:std::sync::mpsc::Receiver<(String,Option<Vec<u8>>)>,
}
impl ArtworkWorker {
    fn start()->Option<Self>{
        let (requests,rx)=std::sync::mpsc::channel::<ArtworkRequest>();
        let (tx,results)=std::sync::mpsc::channel();
        std::thread::Builder::new().name("daozai-artwork".into()).spawn(move||{
            let _apartment=init_winrt();
            while let Ok((key,load))=rx.recv(){if tx.send((key,load())).is_err(){break;}}
        }).ok()?;
        Some(Self{requests,results})
    }
}

#[derive(Default)]
struct ArtworkCache {
    #[cfg(windows)]
    kugou:kugou_metadata::Cache,
    #[cfg(windows)]
    system:Option<(NowPlaying,std::time::Instant)>,
    #[cfg(windows)]
    manager:Option<windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager>,
    key: String,
    bytes: Vec<u8>,
    image: Option<Arc<eframe::egui::ColorImage>>,
    worker: Option<ArtworkWorker>,
    pending: bool,
    next_poll: Option<std::time::Instant>,
}

impl ArtworkCache {
    #[cfg(windows)]
    fn system_result(&mut self,result:Result<Option<NowPlaying>,()>,now:std::time::Instant)->Option<NowPlaying>{
        match result {
            Ok(media)=>{self.system=media.clone().map(|media|(media,now));media},
            Err(())=>self.system.as_ref().filter(|(_,at)|now.duration_since(*at)<=Duration::from_secs(2)).map(|(media,_)|media.clone()),
        }
    }
    fn poll(&mut self,key:String,load:impl FnOnce()->Option<Vec<u8>>+Send+'static)->Option<Arc<eframe::egui::ColorImage>> {
        self.poll_at(key,load,std::time::Instant::now())
    }
    fn poll_at(&mut self,key:String,load:impl FnOnce()->Option<Vec<u8>>+Send+'static,now:std::time::Instant)->Option<Arc<eframe::egui::ColorImage>> {
        self.get(key.clone(),||None);
        if let Some(worker)=&self.worker {
            match worker.results.try_recv() {
                Ok((loaded,bytes))=>{
                    self.pending=false;
                    if loaded==key {self.get(key.clone(),||bytes);}
                },
                Err(std::sync::mpsc::TryRecvError::Disconnected)=>{self.worker=None;self.pending=false;},
                Err(std::sync::mpsc::TryRecvError::Empty)=>{},
            }
        }
        // 最多一个封面读取任务；旧任务结束后只采纳当前歌曲的结果。
        if !self.pending && self.next_poll.is_none_or(|at|now>=at) {
            self.next_poll=Some(now+Duration::from_millis(if self.image.is_some(){2000}else{500}));
            if self.worker.is_none(){self.worker=ArtworkWorker::start();}
            if let Some(worker)=&self.worker{self.pending=worker.requests.send((key,Box::new(load))).is_ok();}
        }
        self.image.clone()
    }
    fn get(&mut self, key:String, load:impl FnOnce()->Option<Vec<u8>>)->Option<Arc<eframe::egui::ColorImage>> {
        if self.key!=key {self.key=key;self.bytes.clear();self.image=None;self.next_poll=None;}
        // 封面可能晚于歌名更新；每次检查内容，只在图片变化时重新解码。
        if let Some(bytes)=load() {
            if bytes!=self.bytes {
                if let Some(image)=decode_artwork(&bytes) {
                    if self.image.as_deref()!=Some(&image){self.image=Some(Arc::new(image));}
                }
                self.bytes=bytes;
            }
        }
        self.image.clone()
    }
}

fn decode_artwork(bytes:&[u8])->Option<eframe::egui::ColorImage> {
    let image=image::load_from_memory(bytes).ok()?.thumbnail(256,256).to_rgba8();
    Some(eframe::egui::ColorImage::from_rgba_unmultiplied([image.width() as usize,image.height() as usize],image.as_raw()))
}

#[cfg(windows)]
fn media_result<T:windows::core::RuntimeType+'static>(operation:windows::Foundation::IAsyncOperation<T>)->Option<T>{
    let start=std::time::Instant::now();
    while operation.Status().ok()?==windows::Foundation::AsyncStatus::Started{
        if start.elapsed()>=Duration::from_millis(500){let _=operation.Cancel();return None;}
        std::thread::sleep(Duration::from_millis(10));
    }
    operation.GetResults().ok()
}

#[cfg(windows)]
fn read_artwork(props:&windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties)->Option<Vec<u8>> {
    use windows::core::Interface;
    use windows::Storage::Streams::DataReader;
    let stream=media_result(props.Thumbnail().ok()?.OpenReadAsync().ok()?)?;
    let size=stream.Size().ok()?;
    if size==0 || size>8*1024*1024 {return None;}
    let reader=DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
    let count=media_result::<u32>(reader.LoadAsync(size as u32).ok()?.cast().ok()?)?;
    if count!=size as u32 {return None;}
    let mut bytes=vec![0;count as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    Some(bytes)
}

pub type Shared = Arc<Mutex<NowPlaying>>;

pub fn shared() -> Shared {
    Arc::new(Mutex::new(NowPlaying::default()))
}

fn choose_media(system:Option<NowPlaying>,native:Option<NowPlaying>)->Option<NowPlaying>{
    if let (Some(system),Some(native))=(&system,&native){
        if system.source.eq_ignore_ascii_case("cloudmusic.exe"){
            let same=system.title==native.title && (system.artist==native.artist || system.artist.is_empty() || native.artist.is_empty());
            let newer=match (native.event_time_ms,system.event_time_ms){(Some(n),Some(s))=>Some(n>s),_=>None};
            let use_native=newer.unwrap_or(same && system.playing==native.playing);
            let mut media=if use_native{native.clone()}else{system.clone()};
            if same {
                let fallback=if use_native{system}else{native};
                if media.artist.is_empty(){media.artist=fallback.artist.clone();}
                if media.duration<=0.0{media.duration=fallback.duration;}
                if media.song_id.is_none(){media.song_id=fallback.song_id;}
                if media.artwork.is_none(){media.artwork=system.artwork.clone().or_else(||native.artwork.clone());}
            }
            return Some(media);
        }
    }
    if system.as_ref().is_some_and(|m|m.playing && m.is_music()){system}
    else if native.as_ref().is_some_and(|m|m.playing && m.is_music()){native}
    else if system.as_ref().is_some_and(NowPlaying::is_music){system}
    else{native.or(system)}
}

fn media_rank(media:&NowPlaying)->u8{
    match (media.is_music(),media.playing){(true,true)=>0,(true,false)=>1,(false,true)=>2,_=>3}
}

fn choose_media_for_source(system:Option<NowPlaying>,native:Option<NowPlaying>,previous:&str,native_pending:bool)->Option<NowPlaying>{
    // 暂停或载入下一首时保留控制对象，其他客户端开始播放则照常切换。
    if previous.eq_ignore_ascii_case("cloudmusic.exe") && !system.as_ref().is_some_and(|m|m.playing && m.is_music())
        && (native.is_some() || native_pending) {
        if native.is_some() && system.as_ref().is_some_and(|m|m.source.eq_ignore_ascii_case(previous)){return choose_media(system,native);}
        return native;
    }
    choose_media(system,native)
}

#[cfg(windows)]
fn query(artwork:&mut ArtworkCache) -> Option<NowPlaying> {
    let result=query_system(artwork);artwork.system_result(result,std::time::Instant::now())
}

#[cfg(windows)]
fn query_system(artwork:&mut ArtworkCache) -> Result<Option<NowPlaying>,()> {
    use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;

    if artwork.manager.is_none(){artwork.manager=media_result(GlobalSystemMediaTransportControlsSessionManager::RequestAsync().map_err(|_|())?);}
    let mgr=artwork.manager.clone().ok_or(())?;
    let current=mgr.GetCurrentSession().ok();
    let mut sessions=current.iter().cloned().collect::<Vec<_>>();
    let all=mgr.GetSessions().map_err(|_|{artwork.manager=None;})?;
    for session in all{if !sessions.contains(&session){sessions.push(session);}}
    if sessions.is_empty(){return Ok(None);}
    // 先比较全部会话；视频不能遮挡音乐，封面只为最终选中的会话读取。
    let previous=artwork.system.as_ref().map(|(media,_)|media.source.clone());
    let mut unreadable_previous=false;
    let (session,mut media)=sessions.iter().filter_map(|session|{
        let media=read_session(session,artwork);
        if media.is_none() && session.SourceAppUserModelId().ok().is_some_and(|id|previous.as_deref()==Some(id.to_string().as_str())){unreadable_previous=true;}
        media.map(|media|(session,media))
    })
        .min_by_key(|(_,media)|media_rank(media)).ok_or(())?;
    // 当前播放器正在重载时，不把暂停中的另一客户端当成用户切换了播放源。
    if media_rank(&media)>0 && unreadable_previous{return Err(());}
    let key=format!("{}|{}|{}|{}",media.source,media.title,media.artist,media.album);
    let session=session.clone();
    let title=media.title.clone();
    media.artwork=artwork.poll(key,move ||{
        let props=media_result(session.TryGetMediaPropertiesAsync().ok()?)?;
        if props.Title().ok()?.to_string()!=title{return None;}read_artwork(&props)
    });
    Ok(Some(media))
}

#[cfg(windows)]
fn read_session(session:&windows::Media::Control::GlobalSystemMediaTransportControlsSession,artwork:&mut ArtworkCache)->Option<NowPlaying>{
    let props=media_result(session.TryGetMediaPropertiesAsync().ok()?)?;
    let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
    let mut artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
    if title.is_empty() {
        return None;
    }
    let mut playing = false;
    let mut position = 0.0;
    let mut duration = 0.0;
    let mut sampled_at=None;
    let mut event_time_ms=None;
    let mut playback_type=props.PlaybackType().ok().and_then(|kind|kind.Value().ok());
    if let Ok(info) = session.GetPlaybackInfo() {
        let info_type=info.PlaybackType().ok().and_then(|kind|kind.Value().ok());
        if playback_type.is_none() || info_type.is_some_and(|kind|kind.0==2){playback_type=info_type;}
        if let Ok(status) = info.PlaybackStatus() {
            playing = status == windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
        }
    }
    if let Ok(tl) = session.GetTimelineProperties() {
        event_time_ms=tl.LastUpdatedTime().ok().and_then(|at|at.UniversalTime.checked_sub(116444736000000000))
            .filter(|at|*at>0).map(|at|at as u64/10000);
        sampled_at=Some(std::time::Instant::now());
        if let Ok(p) = tl.Position() {
            position = p.Duration as f64 / 10_000_000.0;
        }
        // SMTC 只在事件发生时更新 Position；播放中要按 LastUpdatedTime 外推到「现在」。
        if playing {
            if let Ok(at) = tl.LastUpdatedTime() {
                let now = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
                let now = ((now.dwHighDateTime as i64) << 32) | now.dwLowDateTime as i64;
                let lag = (now - at.UniversalTime) as f64 / 10_000_000.0;
                if (0.0..86_400.0).contains(&lag) {
                    position += lag;
                }
            }
        }
        if let Ok(e) = tl.EndTime() {
            duration = e.Duration as f64 / 10_000_000.0;
        }
    }
    let source=session.SourceAppUserModelId().map(|s|s.to_string()).unwrap_or_default();
    let mut has_position=position>0.0 || duration>0.0 || event_time_ms.is_some();
    if source.eq_ignore_ascii_case("cloudmusic.exe") && duration<=0.0 {
        if let Some((current,total))=netease_timeline::timeline() {
            position=current;duration=total;sampled_at=Some(std::time::Instant::now());has_position=true;
        }
    }
    if source.to_ascii_lowercase().contains("kugou") {
        if let Some((singer,total))=artwork.kugou.get(&title,&artist) {
            artist=singer;
            if duration<=0.0{duration=total;}
        }
        if let Some(current)=netease_timeline::kugou_position() {
            position=current;sampled_at=Some(std::time::Instant::now());has_position=true;
        }
    }
    let album=props.AlbumTitle().map(|s|s.to_string()).unwrap_or_default();
    if !has_position{sampled_at=None;}
    let playback_type=match playback_type.map(|kind|kind.0){Some(1)=>MediaKind::Music,Some(2)=>MediaKind::Video,_=>MediaKind::Unknown};
    Some(NowPlaying { title, artist, album, source, song_id:None, playing, position, sampled_at, event_time_ms, duration, artwork:None, playback_type })
}

#[cfg(not(windows))]
fn query(_artwork:&mut ArtworkCache) -> Option<NowPlaying> {
    None
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Transport {
    Toggle,
    Next,
    Prev,
    /// 跳到第几秒。
    Seek(f64),
}

/// 控制当前媒体会话（播放/暂停、上一首、下一首）。
#[cfg(windows)]
pub fn control(cmd: Transport,source:&str) {
    if let Transport::Seek(seconds)=cmd {let _=seek(seconds,source);return;}
    use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager;
    let _apartment=init_winrt();
    let session=GlobalSystemMediaTransportControlsSessionManager::RequestAsync().ok().and_then(media_result)
        .and_then(|mgr|mgr.GetSessions().ok()).and_then(|sessions|sessions.into_iter().find(|session|session.SourceAppUserModelId().ok().is_some_and(|id|id.to_string()==source)));
    let Some(session)=session else{
        if source.eq_ignore_ascii_case("cloudmusic.exe"){netease_playback::control(cmd);}
        return;
    };
    let result = match cmd {
        Transport::Toggle => session.TryTogglePlayPauseAsync(),
        Transport::Next => session.TrySkipNextAsync(),
        Transport::Prev => session.TrySkipPreviousAsync(),
        Transport::Seek(secs) => session.TryChangePlaybackPositionAsync((secs.max(0.0) * 10_000_000.0) as i64),
    };
    if let Ok(op) = result {
        let _ = media_result(op);
    }
}

#[cfg(not(windows))]
pub fn control(_cmd: Transport,_source:&str) {}

/// 跳转在工作线程执行；专用回退必须回读实际位置，避免播放器接口假成功。
#[cfg(windows)]
pub fn seek(seconds:f64,source:&str)->bool {
    if !seconds.is_finite() || seconds<0.0{return false;}
    static LOCK:Mutex<()>=Mutex::new(());
    let Ok(_lock)=LOCK.lock()else{return false;};
    if matches!(source.to_ascii_lowercase().as_str(),"kugou"|"kugou.exe") && kugou_control::seek(seconds){return true;}
    let _apartment=init_winrt();
    use windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager as Manager;
    let session=Manager::RequestAsync().ok().and_then(media_result).and_then(|mgr|mgr.GetSessions().ok())
        .and_then(|sessions|sessions.into_iter().find(|s|s.SourceAppUserModelId().ok().is_some_and(|id|id.to_string().eq_ignore_ascii_case(source))));
    let native=source.eq_ignore_ascii_case("qqmusic.exe");
    let timeline=||->Option<(f64,f64)>{
        let tl=session.as_ref()?.GetTimelineProperties().ok()?;
        Some((tl.Position().ok()?.Duration as f64/10_000_000.0,tl.EndTime().ok()?.Duration as f64/10_000_000.0))
    };
    let duration=timeline().map(|(_,duration)|duration).unwrap_or(0.0);
    let target=if duration>0.0{seconds.min(duration)}else{seconds};
    let enabled=session.as_ref().and_then(|s|s.GetPlaybackInfo().ok()).and_then(|i|i.Controls().ok())
        .and_then(|c|c.IsPlaybackPositionEnabled().ok()).unwrap_or(false);
    let paused_qq=source.eq_ignore_ascii_case("qqmusic.exe") && session.as_ref().and_then(|s|s.GetPlaybackInfo().ok())
        .and_then(|info|info.PlaybackStatus().ok()).is_some_and(|status|status!=windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing);
    let accepted=(!native || enabled) && session.as_ref().and_then(|s|s.TryChangePlaybackPositionAsync((target*10_000_000.0)as i64).ok()).and_then(media_result)==Some(true);
    if !native{return accepted;}
    for system in [true,false] {
        if system {if !accepted{continue;}}else {
            if !player_seek::seek(source,target,duration){return false;}
        }
        let start=std::time::Instant::now();
        let timeout=if system{Duration::from_millis(600)}else{Duration::from_secs(2)};
        let mut pause_requested=false;
        while start.elapsed()<timeout {
            std::thread::sleep(Duration::from_millis(100));
            if timeline().is_some_and(|(current,total)|(total-duration).abs()<1.5 && seek_arrived(current,target,start.elapsed().as_secs_f64())){
                // QQ 的原生进度条会自动续播；确认续播事件后再恢复暂停，避免命令被覆盖。
                if paused_qq && session.as_ref().and_then(|s|s.GetPlaybackInfo().ok()).and_then(|i|i.PlaybackStatus().ok())
                    ==Some(windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing){
                    if !pause_requested {
                        if session.as_ref().and_then(|s|s.TryTogglePlayPauseAsync().ok()).and_then(media_result)!=Some(true){return false;}
                        pause_requested=true;
                    }
                    continue;
                }
                return true;
            }
        }
    }
    false
}

#[cfg(not(windows))]
pub fn seek(_seconds:f64,_source:&str)->bool {false}

fn seek_arrived(current:f64,target:f64,elapsed:f64)->bool {
    current.is_finite() && target.is_finite() && current>=target-3.0 && current<=target+elapsed+3.0
}

#[test]
fn seek_confirmation_rejects_unchanged_position_and_accepts_playback_advance(){
    assert!(!seek_arrived(14.0,27.0,2.0));
    assert!(!seek_arrived(180.0,60.0,2.0));
    assert!(seek_arrived(60.0,60.0,0.2));
    assert!(seek_arrived(62.0,60.0,2.0));
    assert!(!seek_arrived(f64::NAN,60.0,2.0));
}

#[cfg(windows)]
struct MediaApartment(Option<windows::Win32::System::Com::CO_MTA_USAGE_COOKIE>);
#[cfg(windows)]
impl Drop for MediaApartment {fn drop(&mut self){if let Some(cookie)=self.0 {let _=unsafe{windows::Win32::System::Com::CoDecrementMTAUsage(cookie)};}}}
#[cfg(windows)]
fn init_winrt()->MediaApartment {MediaApartment(unsafe{windows::Win32::System::Com::CoIncrementMTAUsage().ok()})}

#[cfg(not(windows))]
fn init_winrt() {}

// 音乐客户端可能重复上报同一播放位置，保留该位置首次采样的时间。
#[derive(Default)]
struct PlaybackClock { previous: Option<NowPlaying> }
impl PlaybackClock {
    fn update(&mut self, media: &mut NowPlaying) {
        if !media.is_music() || media.sampled_at.is_none() {
            self.previous=None;
            return;
        }
        if let Some(previous)=&self.previous {
            if media.playing && previous.playing && media.source==previous.source
                && media.title==previous.title && media.artist==previous.artist && media.album==previous.album
                && media.song_id==previous.song_id
                && media.position==previous.position && previous.sampled_at.is_some() {
                media.sampled_at=previous.sampled_at;
            }
        }
        self.previous=Some(media.clone());
    }
}

#[test]
fn kugou_repeated_samples_do_not_freeze_lyrics() {
    let mut clock=PlaybackClock::default();
    let start=std::time::Instant::now();
    let mut media=NowPlaying{source:"KuGou.exe".into(),title:"歌曲".into(),artist:"歌手".into(),
        position:42.0,playing:true,sampled_at:Some(start),..Default::default()};
    clock.update(&mut media);
    let lyrics=crate::lyrics::parse_lrc("[00:42.00]第一句\n[00:45.00]第二句");
    for second in 1..=8 {
        let now=start+Duration::from_secs(second);
        media.sampled_at=Some(now);
        clock.update(&mut media);
        let position=media.position+now.duration_since(media.sampled_at.unwrap()).as_secs_f64();
        assert_eq!(position,42.0+second as f64,"重复采样不能把进度拉回原地");
        if second>=3 {assert_eq!(lyrics.index_at(position as f32),Some(1));}
    }
    // 新位置、暂停、恢复、切歌和切换播放器必须建立新的采样基准。
    for (position,playing,title,source) in [(12.0,true,"歌曲","KuGou.exe"),(12.0,false,"歌曲","KuGou.exe"),
        (12.0,true,"歌曲","KuGou.exe"),(12.0,true,"新歌","KuGou.exe"),(12.0,true,"新歌","QQMusic.exe")] {
        let now=media.sampled_at.unwrap()+Duration::from_secs(1);
        media.position=position;media.playing=playing;media.title=title.into();media.source=source.into();media.sampled_at=Some(now);
        clock.update(&mut media);
        assert_eq!(media.sampled_at,Some(now));
        assert_eq!(media.position,position);
    }
}

#[test]
fn other_clients_repeated_samples_keep_the_clock_running() {
    for source in ["QQMusic.exe","SodaMusic.exe","Spotify.exe","cloudmusic.exe","msedge.exe"] {
        let mut clock=PlaybackClock::default();
        let start=std::time::Instant::now();
        let mut media=NowPlaying{source:source.into(),title:"歌曲".into(),artist:"歌手".into(),album:"专辑".into(),
            position:10.0,duration:120.0,playing:true,sampled_at:Some(start),..Default::default()};
        clock.update(&mut media);
        for second in 1..=5 {
            media.sampled_at=Some(start+Duration::from_secs(second));
            clock.update(&mut media);
            assert_eq!(media.sampled_at,Some(start),"{source} 重复位置不能重置采样时刻");
        }
        for position in [16.0,17.0,4.0,90.0] {
            let now=start+Duration::from_secs(10);
            media.position=position;media.sampled_at=Some(now);
            clock.update(&mut media);
            assert_eq!(media.sampled_at,Some(now),"{source} 必须跟随新位置");
        }
        for change in 0..7 {
            let now=start+Duration::from_secs(20+change);
            match change {
                0=>media.playing=false,
                1=>media.playing=true,
                2=>media.title="新歌".into(),
                3=>media.artist="另一歌手".into(),
                4=>media.album="另一专辑".into(),
                5=>media.song_id=Some(123),
                _=>media.source="另一个播放器".into(),
            }
            media.sampled_at=Some(now);clock.update(&mut media);
            assert_eq!(media.sampled_at,Some(now),"{source} 状态变化必须重置时钟");
        }
        // 会话消失或缺少有效时间轴后，恢复时不能沿用旧计时。
        clock.update(&mut NowPlaying::default());
        let now=start+Duration::from_secs(30);
        media.sampled_at=Some(now);clock.update(&mut media);assert_eq!(media.sampled_at,Some(now));
        media.sampled_at=None;clock.update(&mut media);assert_eq!(media.sampled_at,None);
        media.sampled_at=Some(now);clock.update(&mut media);assert_eq!(media.sampled_at,Some(now));
    }
}

#[derive(Default)]
struct MediaMemory {last:Option<(NowPlaying,std::time::Instant)>}
impl MediaMemory {
    fn update(&mut self,media:&mut NowPlaying,now:std::time::Instant){
        if media.playback_type==MediaKind::Video{self.last=None;return;}
        if media.title.trim().is_empty(){
            // 会话重建时可短暂返回空值；保留窗口不刷新期限，真正退出最多延迟两秒收起。
            if let Some((old,at))=&self.last {
                if now.duration_since(*at)<=Duration::from_secs(2) && (media.source.is_empty() || media.source==old.source){
                    *media=old.clone();return;
                }
            }
            self.last=None;return;
        }
        let complete=!media.artist.trim().is_empty() && media.duration>0.0;
        if let Some((old,at))=&self.last {
            let same=media.source==old.source && media.title==old.title && media.song_id==old.song_id
                && (media.artist.trim().is_empty() || crate::lyrics::artist_set(&media.artist)==crate::lyrics::artist_set(&old.artist));
            if same && now.duration_since(*at)<=Duration::from_secs(2) {
                if media.artist.trim().is_empty(){media.artist=old.artist.clone();}
                if media.duration<=0.0{media.duration=old.duration;}
                // 同一歌曲的秒数舍入波动不代表换歌，避免重置歌词和重复请求。
                if (media.duration-old.duration).abs()<=1.0{media.duration=old.duration;}
            }else if !same {self.last=None;}
        }
        if complete{self.last=Some((media.clone(),now));}
    }
}

/// 后台线程：刷新正在播放，进度条在两次刷新之间由界面按时间外推。
pub fn spawn_poller(state: Shared,ctx:eframe::egui::Context)->Arc<std::sync::atomic::AtomicBool> {
    let enabled=Arc::new(std::sync::atomic::AtomicBool::new(true));
    let active=enabled.clone();
    std::thread::spawn(move || {
        let _apartment=init_winrt();
        let mut artwork=ArtworkCache::default();
        let mut playback_clock=PlaybackClock::default();
        let mut memory=MediaMemory::default();
        let mut selected_source=String::new();
        #[cfg(windows)]
        let mut netease=netease_playback::Reader::default();
        loop {
            if Arc::strong_count(&state)==1{break;}
            if !active.load(std::sync::atomic::Ordering::Relaxed){
                artwork=ArtworkCache::default();playback_clock=PlaybackClock::default();memory=MediaMemory::default();selected_source.clear();
                #[cfg(windows)] {netease=netease_playback::Reader::default();}
                if let Ok(mut current)=state.lock(){if !current.title.is_empty(){*current=NowPlaying::default();ctx.request_repaint();}}
                std::thread::sleep(Duration::from_millis(250));continue;
            }
            let system=query(&mut artwork);
            #[cfg(windows)]
            let native=netease.poll();
            #[cfg(windows)]
            let native_pending=netease.pending();
            #[cfg(not(windows))]
            let native=None;
            #[cfg(not(windows))]
            let native_pending=false;
            let mut np=choose_media_for_source(system,native,&selected_source,native_pending).unwrap_or_default();
            memory.update(&mut np,std::time::Instant::now());
            if !np.source.is_empty(){selected_source=np.source.clone();}
            #[cfg(windows)]
            netease.fill_artwork(&mut np);
            playback_clock.update(&mut np);
            if np.title.is_empty() {
                #[cfg(windows)] let manager=artwork.manager.take();
                artwork=ArtworkCache::default();
                #[cfg(windows)] {artwork.manager=manager;}
            }
            if let Ok(mut cur) = state.lock() {
                let changed=cur.title!=np.title || cur.artist!=np.artist || cur.playing!=np.playing
                    || cur.source!=np.source || cur.song_id!=np.song_id || cur.duration!=np.duration
                    || cur.artwork.as_ref().zip(np.artwork.as_ref()).map_or(cur.artwork.is_some()!=np.artwork.is_some(),|(old,new)|!Arc::ptr_eq(old,new));
                *cur = np;
                if changed{ctx.request_repaint();}
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    });
    enabled
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_native_event_preserves_matching_system_metadata(){
        let system=NowPlaying{source:"cloudmusic.exe".into(),title:"歌曲".into(),artist:"歌手".into(),duration:120.0,event_time_ms:Some(1000),..Default::default()};
        let native=NowPlaying{artist:String::new(),duration:0.0,event_time_ms:Some(2000),playing:true,position:12.0,..system.clone()};
        for previous in ["","cloudmusic.exe"] {
            let selected=choose_media_for_source(Some(system.clone()),Some(native.clone()),previous,false).unwrap();
            assert_eq!(selected.artist,"歌手");
            assert_eq!(selected.duration,120.0);
            assert!(selected.playing);assert_eq!(selected.position,12.0);
        }
    }

    #[test]
    fn unchanged_cover_does_not_start_a_worker_on_every_poll(){
        let mut cache=ArtworkCache::default();
        let now=std::time::Instant::now();
        let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..30 {
            let calls=calls.clone();
            cache.poll_at("same-song".into(),move ||{calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Some(cover_bytes(eframe::egui::Color32::RED))},now);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(cache.image.is_some(),"首张封面仍应及时加载");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),1,"同一封面不能每次刷新都创建线程和读取系统流");
    }

    #[test]
    fn changing_tracks_reuses_the_cover_worker_without_waiting_for_old_backoff(){
        let mut cache=ArtworkCache::default();let mut threads=std::collections::HashSet::new();
        for song in ["first","second","third"] {
            let (tx,rx)=std::sync::mpsc::channel();
            cache.poll(song.into(),move||{tx.send(std::thread::current().id()).unwrap();Some(cover_bytes(eframe::egui::Color32::BLUE))});
            threads.insert(rx.recv_timeout(Duration::from_secs(2)).expect("切歌后应立即读取新封面"));
            let start=std::time::Instant::now();
            while cache.image.is_none(){
                assert!(start.elapsed()<Duration::from_secs(2));
                std::thread::sleep(Duration::from_millis(5));cache.poll(song.into(),||None);
            }
        }
        assert_eq!(threads.len(),1,"连续切歌应复用同一个读取线程");
    }

    #[test]
    #[ignore="需要本机存在媒体会话；只读验证关闭音乐页后停止采样、重新打开后恢复"]
    fn live_media_sampling_can_be_disabled_and_resumed(){
        let state=shared();let enabled=spawn_poller(state.clone(),eframe::egui::Context::default());
        let wait=|present:bool|{
            let start=std::time::Instant::now();
            while state.lock().unwrap().title.is_empty()==present {
                assert!(start.elapsed()<Duration::from_secs(10),"媒体采样开关没有生效");
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        wait(true);enabled.store(false,std::sync::atomic::Ordering::Relaxed);wait(false);
        std::thread::sleep(Duration::from_millis(500));assert!(state.lock().unwrap().title.is_empty());
        enabled.store(true,std::sync::atomic::Ordering::Relaxed);wait(true);
    }

    #[test]
    fn native_pause_and_loading_do_not_select_another_paused_player(){
        let native=NowPlaying{source:"cloudmusic.exe".into(),title:"网易云歌曲".into(),artist:"歌手".into(),..Default::default()};
        for source in ["QQMusic.exe","kugou","汽水音乐"] {
            let other=NowPlaying{source:source.into(),title:"其他歌曲".into(),artist:"歌手".into(),..Default::default()};
            assert_eq!(choose_media_for_source(Some(other.clone()),Some(native.clone()),"cloudmusic.exe",false).unwrap().source,"cloudmusic.exe","暂停后必须仍能控制原播放器");
            assert!(choose_media_for_source(Some(other.clone()),None,"cloudmusic.exe",true).is_none(),"加载期间不能把按钮指向其他播放器");
            assert_eq!(choose_media_for_source(Some(other.clone()),None,"cloudmusic.exe",false).unwrap().source,source,"退出后允许回到系统会话");
            let mut playing=other;playing.playing=true;
            assert_eq!(choose_media_for_source(Some(playing),Some(native.clone()),"cloudmusic.exe",false).unwrap().source,source,"其他客户端开始播放时应正常切换");
        }
    }

    #[test]
    fn slow_cover_does_not_block_metadata_or_replace_a_new_song(){
        let mut cache=ArtworkCache::default();
        let (release,wait)=std::sync::mpsc::channel();
        let start=std::time::Instant::now();
        let unblock=std::thread::spawn(move ||{std::thread::sleep(Duration::from_millis(150));release.send(()).unwrap();});
        assert!(cache.poll("old".into(),move ||{wait.recv().unwrap();Some(cover_bytes(eframe::egui::Color32::RED))}).is_none());
        assert!(start.elapsed()<Duration::from_millis(100),"封面读取不能阻塞歌曲刷新");
        assert!(cache.poll("new".into(),||None).is_none());
        unblock.join().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        assert!(cache.poll("new".into(),||None).is_none(),"旧歌曲的迟到封面不能覆盖新歌曲");
    }

    #[test]
    fn native_cover_survives_a_missing_system_thumbnail(){
        let cover=Arc::new(eframe::egui::ColorImage::new([2,2],eframe::egui::Color32::BLUE));
        let native=NowPlaying{source:"cloudmusic.exe".into(),title:"歌曲".into(),artist:"歌手".into(),playing:true,artwork:Some(cover.clone()),..Default::default()};
        let mut system=native.clone();system.artwork=None;
        assert!(Arc::ptr_eq(choose_media(Some(system),Some(native)).unwrap().artwork.as_ref().expect("系统封面缺失时应保留原生封面"),&cover));
    }

    #[test]
    fn other_music_clients_keep_their_own_covers_when_netease_is_open(){
        let cover=Arc::new(eframe::egui::ColorImage::new([2,2],eframe::egui::Color32::RED));
        let native=NowPlaying{source:"cloudmusic.exe".into(),title:"网易云歌曲".into(),artist:"歌手".into(),playing:true,
            artwork:Some(Arc::new(eframe::egui::ColorImage::new([2,2],eframe::egui::Color32::BLUE))),..Default::default()};
        for source in ["SodaMusic.exe","KuGou.exe","QQMusic.exe","Spotify.exe"]{
            let system=NowPlaying{source:source.into(),title:"当前歌曲".into(),artist:"歌手".into(),playing:true,artwork:Some(cover.clone()),..Default::default()};
            let selected=choose_media(Some(system.clone()),Some(native.clone())).unwrap();
            assert_eq!(selected.source,source);assert!(Arc::ptr_eq(selected.artwork.as_ref().unwrap(),&cover));
            let mut paused=system;paused.playing=false;let mut native_paused=native.clone();native_paused.playing=false;
            assert!(Arc::ptr_eq(choose_media(Some(paused),Some(native_paused)).unwrap().artwork.as_ref().unwrap(),&cover));
            assert_eq!(choose_media(None,Some(native.clone())).unwrap().source,"cloudmusic.exe");
        }
    }

    #[test]
    fn native_music_recovers_after_the_previous_player_closes(){
        let netease=NowPlaying{source:"cloudmusic.exe".into(),title:"歌曲".into(),artist:"歌手".into(),playing:true,position:42.0,duration:240.0,..Default::default()};
        assert_eq!(choose_media(None,Some(netease.clone())).unwrap().source,"cloudmusic.exe","网易云未注册 SMTC 时仍应接回正在播放的歌曲");
        let old=NowPlaying{source:"SodaMusic.exe".into(),title:"上一首".into(),artist:"歌手".into(),..Default::default()};
        assert_eq!(choose_media(Some(old.clone()),Some(netease.clone())).unwrap().source,"cloudmusic.exe","旧客户端的暂停会话不能挡住正在播放的网易云");
        let mut current=old.clone();current.playing=true;
        assert_eq!(choose_media(Some(current),Some(netease)).unwrap().source,"SodaMusic.exe","正常播放的系统当前会话应保持优先");
        assert!(choose_media(None,None).is_none(),"播放器全部退出后不能残留歌曲");
    }

    fn cover_bytes(color:eframe::egui::Color32)->Vec<u8> {
        let image=image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(2,2,image::Rgba(color.to_array())));
        let mut bytes=std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes,image::ImageFormat::Png).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn known_music_clients_accept_missing_artist_but_not_video(){
        for source in ["kugou","QQMusic.exe","cloudmusic.exe","汽水音乐","SodaMusic.exe","Spotify.exe","kwmusic.exe"] {
            let mut media=NowPlaying{source:source.into(),title:"歌曲".into(),playback_type:MediaKind::Music,..Default::default()};
            assert!(media.is_music(),"{source} 缺少歌手时仍应显示歌曲");
            media.playback_type=MediaKind::Video;assert!(!media.is_music());
        }
    }

    #[cfg(windows)]
    #[test]
    fn transient_system_failure_is_not_the_same_as_player_exit(){
        let mut cache=ArtworkCache::default();let start=std::time::Instant::now();
        let media=NowPlaying{title:"歌".into(),source:"QQMusic.exe".into(),playback_type:MediaKind::Music,..Default::default()};
        cache.system_result(Ok(Some(media)),start);
        assert_eq!(cache.system_result(Err(()),start+Duration::from_secs(1)).unwrap().title,"歌");
        assert!(cache.system_result(Err(()),start+Duration::from_secs(3)).is_none());
        cache.system_result(Ok(None),start+Duration::from_secs(3));
        assert!(cache.system_result(Err(()),start+Duration::from_secs(4)).is_none());
    }

    #[test]
    fn active_music_wins_over_video_and_stale_paused_sessions(){
        let music=NowPlaying{source:"QQMusic.exe".into(),title:"歌".into(),playback_type:MediaKind::Music,playing:true,..Default::default()};
        let video=NowPlaying{source:"Chrome".into(),title:"视频".into(),playback_type:MediaKind::Video,playing:true,..Default::default()};
        let paused=NowPlaying{playing:false,..music.clone()};
        for sessions in [vec![video.clone(),paused.clone(),music.clone()],vec![paused.clone(),music.clone(),video.clone()]] {
            let selected=sessions.iter().min_by_key(|m|media_rank(m)).unwrap();assert!(selected.is_music() && selected.playing);
        }
        assert!(choose_media(Some(video),Some(paused)).unwrap().is_music());
    }

    #[test]
    fn qq_session_dropouts_and_duration_rounding_keep_one_track(){
        let start=std::time::Instant::now();let mut memory=MediaMemory::default();
        let song=NowPlaying{source:"QQMusic.exe".into(),title:"灰色的夜".into(),artist:"歌手".into(),duration:240.4,playing:true,playback_type:MediaKind::Music,..Default::default()};
        for step in 0..1440 {
            let mut sample=match step%3 {
                0=>song.clone(),
                1=>NowPlaying::default(),
                _=>NowPlaying{duration:240.6,..song.clone()},
            };
            memory.update(&mut sample,start+Duration::from_millis(step*250));
            assert_eq!(sample.title,song.title,"短暂丢失会话不能让歌词条反复收起");
            assert_eq!(sample.duration,song.duration,"时长舍入抖动不能重新取词");
        }
        let mut paused=NowPlaying{playing:false,..song.clone()};
        memory.update(&mut paused,start+Duration::from_secs(360));assert!(!paused.playing);
        let mut gone=NowPlaying::default();
        memory.update(&mut gone,start+Duration::from_secs(363));assert!(gone.title.is_empty());
        let mut next=NowPlaying{title:"下一首".into(),..song};
        memory.update(&mut next,start+Duration::from_secs(364));assert_eq!(next.title,"下一首");
    }

    #[test]
    fn metadata_gap_does_not_clear_lyrics_or_hide_loop_reset(){
        let start=std::time::Instant::now();let mut memory=MediaMemory::default();
        let full=NowPlaying{source:"QQMusic.exe".into(),title:"歌".into(),artist:"歌手".into(),duration:120.0,position:119.0,playing:true,playback_type:MediaKind::Music,..Default::default()};
        memory.update(&mut full.clone(),start);
        let mut gap=NowPlaying{artist:String::new(),duration:0.0,position:0.0,..full.clone()};
        memory.update(&mut gap,start+Duration::from_secs(1));assert_eq!(gap.artist,"歌手");assert_eq!(gap.duration,120.0);assert_eq!(gap.position,0.0);
        let mut expired=NowPlaying{artist:String::new(),duration:0.0,..full.clone()};
        memory.update(&mut expired,start+Duration::from_secs(3));assert!(expired.artist.is_empty());
        let mut other=NowPlaying{title:"另一首".into(),artist:String::new(),duration:0.0,..full};
        memory.update(&mut other,start+Duration::from_millis(1500));assert!(other.artist.is_empty());
        memory.update(&mut NowPlaying::default(),start+Duration::from_secs(4));assert!(memory.last.is_none());
    }

    #[test]
    fn netease_reconciliation_uses_event_time_not_poll_time(){
        let system=NowPlaying{source:"cloudmusic.exe".into(),title:"新歌".into(),artist:"歌手".into(),playing:true,event_time_ms:Some(2000),..Default::default()};
        let mut native=NowPlaying{title:"旧歌".into(),event_time_ms:Some(1000),..system.clone()};
        assert_eq!(choose_media(Some(system.clone()),Some(native.clone())).unwrap().title,"新歌");
        native.event_time_ms=Some(3000);native.title="最新歌曲".into();
        assert_eq!(choose_media(Some(system),Some(native)).unwrap().title,"最新歌曲");
    }

    #[test]
    fn stale_netease_metadata_does_not_replace_new_system_track(){
        let old=NowPlaying{source:"cloudmusic.exe".into(),title:"旧歌".into(),artist:"歌手".into(),..Default::default()};
        let new=NowPlaying{title:"新歌".into(),playing:true,..old.clone()};
        let result=choose_media(Some(new),Some(old)).unwrap();assert_eq!(result.title,"新歌");assert!(result.playing);
    }

    #[test]
    fn kugou_music_without_artist_is_still_a_song(){
        let mut np=NowPlaying{source:"kugou".into(),title:"想你就写信 (你说想哭就弹琴)".into(),playing:true,playback_type:MediaKind::Music,..Default::default()};
        assert!(np.is_music(),"酷狗漏报歌手不能清空正在播放的音乐");
        np.playback_type=MediaKind::Video;assert!(!np.is_music());
        np.playback_type=MediaKind::Music;np.title.clear();assert!(!np.is_music());
    }

    #[test]
    fn media_kind_checks_video_type_before_song_metadata(){
        let mut np=NowPlaying{title:"Video title".into(),artist:"Video author".into(),playback_type:MediaKind::Video,..Default::default()};
        assert_eq!(np.kind(),MediaKind::Video);
        np.playback_type=MediaKind::Music;np.title="小唐的Token的抖音 - 抖音".into();
        assert_eq!(np.kind(),MediaKind::Video,"浏览器错误上报音乐类型时仍应识别明确的视频标题");
        np.title="Movie - YouTube".into();assert_eq!(np.kind(),MediaKind::Video);
        np.title="阿拉斯加海湾".into();np.artist="蓝心羽".into();assert!(np.is_music());
        np.artist.clear();assert_eq!(np.kind(),MediaKind::Unknown,"只有页面标题不能确认是歌曲");
    }

    #[test]
    fn browser_video_authors_are_not_enough_to_identify_a_song(){
        let mut np=NowPlaying{title:"A video".into(),artist:"Video creator".into(),source:"Chrome".into(),playback_type:MediaKind::Music,..Default::default()};
        assert_eq!(np.kind(),MediaKind::Unknown);
        np.album="YouTube".into();assert_eq!(np.kind(),MediaKind::Video);
        np.title="阿拉斯加海湾".into();np.artist="蓝心羽".into();np.album="阿拉斯加海湾".into();
        assert_eq!(np.kind(),MediaKind::Music,"网易云网页版的完整歌曲元数据仍应识别为音乐");
        np.source="Spotify.exe".into();np.album.clear();assert_eq!(np.kind(),MediaKind::Music,"音乐客户端不强制要求专辑信息");
    }

    #[test]
    fn artwork_cache_retries_late_images_and_clears_on_song_change(){
        let mut cache=ArtworkCache::default();
        assert!(cache.get("first".into(),||None).is_none());
        let first=cache.get("first".into(),||Some(cover_bytes(eframe::egui::Color32::RED))).unwrap();
        let cached=cache.get("first".into(),||Some(cover_bytes(eframe::egui::Color32::RED))).unwrap();
        assert!(Arc::ptr_eq(&first,&cached));
        let missing=cache.get("first".into(),||None).unwrap();
        assert!(Arc::ptr_eq(&first,&missing),"短暂缺失时应保留当前歌曲封面");
        let invalid=cache.get("first".into(),||Some(b"invalid image".to_vec())).unwrap();
        assert!(Arc::ptr_eq(&first,&invalid),"损坏的缩略图不应清空当前有效封面");
        assert!(cache.get("second".into(),||None).is_none(),"切歌后不能保留上一首封面");
    }

    #[test]
    fn artwork_cache_replaces_a_temporary_cover_on_the_same_song(){
        let mut cache=ArtworkCache::default();
        let temporary=cache.get("song".into(),||Some(cover_bytes(eframe::egui::Color32::RED))).unwrap();
        // 网页可能先提供占位图，随后才发布真实封面。
        let actual=cache.get("song".into(),||Some(cover_bytes(eframe::egui::Color32::BLUE))).unwrap();
        assert!(!Arc::ptr_eq(&temporary,&actual),"同一首歌的真实封面到达后必须替换占位图");
        assert_eq!(actual.pixels[0],eframe::egui::Color32::BLUE);
    }

    #[test]
    fn artwork_decodes_png_jpeg_webp_and_rejects_invalid_bytes(){
        let original=image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(600,300,image::Rgb([24,120,210])));
        for format in [image::ImageFormat::Png,image::ImageFormat::Jpeg,image::ImageFormat::WebP] {
            let mut bytes=std::io::Cursor::new(Vec::new());
            original.write_to(&mut bytes,format).unwrap();
            let decoded=decode_artwork(bytes.get_ref()).unwrap();
            assert_eq!(decoded.size,[256,128]);
            assert!(decoded.pixels.iter().all(|p|p.a()==255));
        }
        assert!(decode_artwork(b"invalid image").is_none());
    }

    #[cfg(windows)]
    #[test]
    #[ignore="会连续切换当前播放器的四首歌曲，仅用于本机验收"]
    fn live_rapid_track_changes(){
        let state=shared();spawn_poller(state.clone(),eframe::egui::Context::default());
        std::thread::sleep(Duration::from_secs(2));
        let first=state.lock().unwrap().clone();
        assert!(first.is_music() && first.playing,"请先播放音乐");
        let expected=std::env::var("DAOZAI_EXPECT_SOURCE").expect("请指定实测客户端").to_lowercase();
        assert!(first.source.to_lowercase().contains(&expected),"正在播放的客户端不符：{}",first.source);
        for _ in 0..4 {
            control(Transport::Next,&first.source);
            std::thread::sleep(Duration::from_millis(400));
        }
        let start=std::time::Instant::now();
        let mut requested=String::new();
        let mut result=None;
        let mut fetched=None;
        let mut observed=String::new();
        let mut stable_since=std::time::Instant::now();
        loop {
            let media=state.lock().unwrap().clone();
            let key=format!("{}|{}|{}|{:.0}|{:?}",media.source,media.title,media.artist,media.duration,media.song_id);
            if observed!=key {observed=key.clone();stable_since=std::time::Instant::now();}
            if media.source==first.source && media.title!=first.title && requested!=key && stable_since.elapsed()>Duration::from_secs(2) {
                requested=key;
                println!("取词：{} / {} 时长={:.0}",media.title,media.artist,media.duration);
                fetched=None;
                let (tx,rx)=std::sync::mpsc::channel();result=Some(rx);
                let song=media.clone();
                std::thread::spawn(move ||{let _=tx.send(crate::lyrics::fetch_for_source(&song.source,&song.title,&song.artist,song.duration,song.song_id));});
            }
            let native=crate::lyrics::read_native(&media.source,&media.title,&media.artist,media.duration);
            if let Some(outcome)=result.as_ref().and_then(|rx|rx.try_recv().ok()) {
                match outcome {Ok(lyrics)=>fetched=Some(lyrics),Err(error)=>println!("取词结果：{error:?}")}
            }
            let lyrics=native.or_else(||fetched.clone());
            if media.source==first.source && media.title!=first.title && stable_since.elapsed()>Duration::from_secs(2) && media.artwork.is_some() && lyrics.as_ref().is_some_and(|l|l.synced) {
                println!("连续切歌后 {:.2}s：客户端={} 歌曲={} 封面已就绪 歌词={}行",start.elapsed().as_secs_f64(),media.source,media.title,lyrics.unwrap().lines.len());
                break;
            }
            assert!(start.elapsed()<Duration::from_secs(20),"切歌后未恢复：{} cover={}",media.title,media.artwork.is_some());
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore="按指定来源发送一次播放控制，仅用于本机验收"]
    fn live_client_transport(){
        let source=std::env::var("DAOZAI_CONTROL_SOURCE").unwrap();
        let cmd=match std::env::var("DAOZAI_CONTROL").unwrap().as_str(){"next"=>Transport::Next,"prev"=>Transport::Prev,"toggle"=>Transport::Toggle,_=>panic!("未知控制")};
        control(cmd,&source);
    }

    #[cfg(windows)]
    #[test]
    #[ignore="真实客户端进度跳转验收；结束后恢复位置，需要 DAOZAI_CONTROL_SOURCE"]
    fn live_client_seek(){
        use windows::Win32::{Foundation::POINT,UI::WindowsAndMessaging::{GetCursorPos,GetForegroundWindow}};
        let _apartment=init_winrt();
        let source=std::env::var("DAOZAI_CONTROL_SOURCE").unwrap();
        let manager=media_result(windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync().unwrap()).unwrap();
        let snapshot=||{
            let mut rows=Vec::new();
            for session in manager.GetSessions().unwrap(){
                if let Some(media)=read_session(&session,&mut ArtworkCache::default()) {rows.push(media);}
            }
            if let Some(media)=netease_playback::Reader::default().poll(){rows.retain(|m|!m.source.eq_ignore_ascii_case("cloudmusic.exe"));rows.push(media);}
            rows
        };
        let before=snapshot();
        let first=before.iter().find(|m|m.source.eq_ignore_ascii_case(&source)).expect("请先打开指定播放器并载入歌曲");
        println!("before {} position={:.2} playing={}",first.source,first.position,first.playing);
        let start=std::time::Instant::now();
        let foreground=unsafe{GetForegroundWindow()};let mut cursor=POINT::default();unsafe{GetCursorPos(&mut cursor).unwrap();}
        let mut success=true;
        for target in [first.duration*0.23,first.duration*0.67] {
            let ok=seek(target,&source);
            let after=snapshot();let current=after.iter().find(|m|m.source.eq_ignore_ascii_case(&source)).unwrap();
            println!("{source} target={target:.2} actual={:.2} playing={} confirmed={ok}",current.position,current.playing);
            success &= ok && current.title==first.title && current.playing==first.playing && (current.position-target).abs()<4.0;
            for other in before.iter().filter(|m|!m.source.eq_ignore_ascii_case(&source)) {
                let next=after.iter().find(|m|m.source==other.source).unwrap();
                let expected=other.position+if other.playing{start.elapsed().as_secs_f64()}else{0.0};
                success &= next.title==other.title && next.playing==other.playing && (next.position-expected).abs()<4.0;
            }
        }
        let restore=(first.position+if first.playing{start.elapsed().as_secs_f64()}else{0.0}).min(first.duration-1.0);
        let restored=seek(restore,&source);
        let mut after_cursor=POINT::default();unsafe{GetCursorPos(&mut after_cursor).unwrap();}
        println!("restore={restore:.2} confirmed={restored}; foreground_unchanged={} cursor_unchanged={}",unsafe{GetForegroundWindow()}==foreground,cursor==after_cursor);
        assert!(success && restored,"实际进度、播放状态或其他客户端状态不符合预期");
        assert_eq!(unsafe{GetForegroundWindow()},foreground,"跳转不能抢占前台");
        assert_eq!(cursor,after_cursor,"跳转不能移动系统鼠标");
    }

    #[cfg(windows)]
    #[test]
    #[ignore="需要网易云及其他播放器同时运行，验证控制不串台"]
    fn live_netease_controls_are_isolated(){
        let _apartment=init_winrt();
        let snapshot=||{
            let mgr=media_result(windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync().unwrap()).unwrap();
            let mut rows=Vec::new();
            for session in mgr.GetSessions().unwrap() {
                let source=session.SourceAppUserModelId().unwrap().to_string();
                if source.eq_ignore_ascii_case("cloudmusic.exe"){continue;}
                let props=media_result(session.TryGetMediaPropertiesAsync().unwrap()).unwrap();
                rows.push((source,props.Title().unwrap().to_string(),session.GetPlaybackInfo().unwrap().PlaybackStatus().unwrap().0));
            }
            rows.sort();rows
        };
        let mut reader=netease_playback::Reader::default();
        for command in [Transport::Next,Transport::Prev,Transport::Toggle,Transport::Toggle] {
            std::thread::sleep(Duration::from_millis(600));
            let before=reader.poll().expect("请先打开网易云并载入歌曲");
            let others=snapshot();
            let foreground=unsafe {windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()};
            control(command,"cloudmusic.exe");
            let start=std::time::Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(100));
                assert_eq!(snapshot(),others,"网易云控制不能改变其他客户端的歌曲或播放状态");
                assert!(start.elapsed()<Duration::from_secs(8),"网易云没有响应 {command:?}");
                let Some(after)=reader.poll() else{continue;};
                let changed=match command {Transport::Toggle=>after.playing!=before.playing,_=>after.song_id!=before.song_id};
                if changed {println!("{command:?}：{} -> {}，playing={}，其他客户端未变化",before.title,after.title,after.playing);break;}
            }
            assert_eq!(unsafe {windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()},foreground,"后台控制不能抢占焦点");
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore="暂停指定客户端之外的系统媒体会话，隔离本机播放验收"]
    fn live_pause_other_clients(){
        let _apartment=init_winrt();
        let keep=std::env::var("DAOZAI_CONTROL_SOURCE").unwrap();
        let manager=media_result(windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync().unwrap()).unwrap();
        for session in manager.GetSessions().unwrap() {
            let source=session.SourceAppUserModelId().unwrap().to_string();
            if source!=keep {
                if let Ok(op)=session.TryPauseAsync(){println!("暂停 {source}: {:?}",media_result(op));}
            }
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore="需要读取当前系统媒体会话"]
    fn live_sessions_metadata(){
        let _apartment=init_winrt();
        let manager=windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager::RequestAsync().unwrap().get().unwrap();
        for session in manager.GetSessions().unwrap() {
            let props=session.TryGetMediaPropertiesAsync().unwrap().get().unwrap();
            println!("source={} title={} artist={} type={:?} state={:?} duration={:?}",session.SourceAppUserModelId().unwrap(),props.Title().unwrap(),props.Artist().unwrap(),props.PlaybackType().ok().and_then(|kind|kind.Value().ok()),session.GetPlaybackInfo().unwrap().PlaybackStatus(),session.GetTimelineProperties().unwrap().EndTime());
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore="只读验证当前酷狗歌曲的补全信息和进度"]
    fn live_kugou_complete_timeline(){
        let _apartment=init_winrt();let mut cache=ArtworkCache::default();
        let start=std::time::Instant::now();
        let first=query(&mut cache).expect("需要正在播放的酷狗会话");
        println!("歌曲={} 歌手={} 曲长={} 进度={} 播放={} 读取耗时={:?}",first.title,first.artist,first.duration,first.position,first.playing,start.elapsed());
        assert!(first.source.eq_ignore_ascii_case("kugou") && first.is_music());
        assert!(first.duration>0.0 && first.position<=first.duration);
        if let Ok(expected)=std::env::var("DAOZAI_EXPECT_DURATION"){assert!((first.duration-expected.parse::<f64>().unwrap()).abs()<1.0);}
        std::thread::sleep(Duration::from_secs(3));
        let next=query(&mut cache).unwrap();assert_eq!(next.title,first.title);
        assert_eq!(next.duration,first.duration);
        if first.playing && next.playing {assert!(next.position>first.position+1.0,"播放进度必须递增");}
        else if !first.playing && !next.playing {assert!((next.position-first.position).abs()<0.2,"暂停不能继续计时");}
        println!("三秒后：进度={} 播放={}",next.position,next.playing);
    }

    #[cfg(windows)]
    #[test]
    #[ignore="验证酷狗实际前进、后退及跳转后歌词，需保持酷狗播放"]
    fn live_kugou_seek_changes_actual_position(){
        let _apartment=init_winrt();let mut cache=ArtworkCache::default();
        let first=query(&mut cache).unwrap();assert_eq!(first.source,"kugou");
        let foreground=unsafe{windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()};
        for target in [15.0,42.0,6.0] {
            control(Transport::Seek(target),&first.source);
            let start=std::time::Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let media=query(&mut cache).unwrap();
                assert_eq!(media.title,first.title);
                if (media.position-target).abs()<2.5 {
                    let lyrics=crate::lyrics::read_native(&media.source,&media.title,&media.artist,media.duration).unwrap();
                    assert!(lyrics.synced);println!("跳转 {target:.0}s，实际 {:.2}s，歌词 {} 行",media.position,lyrics.lines.len());break;
                }
                assert!(start.elapsed()<Duration::from_secs(4),"酷狗未执行跳转：目标={target}，实际={}",media.position);
            }
            assert_eq!(unsafe{windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow()},foreground,"跳转不应抢占前台");
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore="将当前酷狗歌曲跳至尾声，验证真实单曲循环后仍有原生歌词"]
    fn live_kugou_repeat_retains_lyrics(){
        let _apartment=init_winrt();let mut cache=ArtworkCache::default();
        let first=query(&mut cache).unwrap();assert_eq!(first.source,"kugou");assert!(first.playing && first.duration>5.0);
        assert!(crate::lyrics::read_native(&first.source,&first.title,&first.artist,first.duration).is_some());
        control(Transport::Seek(first.duration-3.0),&first.source);
        std::thread::sleep(Duration::from_millis(500));
        let mut previous=query(&mut cache).unwrap().position;
        println!("尾声：{} / {}，进度={previous}，曲长={}",first.title,first.artist,first.duration);
        if previous<=first.duration-5.0{println!("酷狗未接受跳转，等待歌曲自然循环");}
        let start=std::time::Instant::now();let mut wrapped=None;
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let media=query(&mut cache).unwrap();
            assert_eq!((&media.title,&media.artist),(&first.title,&first.artist),"循环前后歌曲身份必须一致");
            assert_eq!(media.duration,first.duration);
            let lyrics=crate::lyrics::read_native(&media.source,&media.title,&media.artist,media.duration).expect("循环前后原生歌词不能消失");
            if media.position+1.0<previous {wrapped=Some(std::time::Instant::now());println!("已循环到开头：进度={}",media.position);}
            if wrapped.is_some_and(|at|at.elapsed()>Duration::from_secs(8)) {
                println!("第二遍：进度={}，歌词={}行，当前={:?}",media.position,lyrics.lines.len(),lyrics.index_at(media.position as f32).map(|i|&lyrics.lines[i].text));break;
            }
            previous=media.position;assert!(start.elapsed().as_secs_f64()<first.duration+30.0,"未观察到单曲循环");
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore="需要指定 DAOZAI_EXPECT_SOURCE，并在本机播放该客户端的音乐"]
    fn live_music_client_cover_lyrics_and_progress(){
        let expected=std::env::var("DAOZAI_EXPECT_SOURCE").expect("请指定实测客户端").to_lowercase();
        let state=shared();spawn_poller(state.clone(),eframe::egui::Context::default());
        let start=std::time::Instant::now();
        let first=loop{
            let media=state.lock().unwrap().clone();
            if media.source.to_lowercase().contains(&expected) && media.playing && media.artwork.is_some(){break media;}
            assert!(start.elapsed()<Duration::from_secs(15),"客户端未就绪：source={} title={} playing={} cover={}",media.source,media.title,media.playing,media.artwork.is_some());
            std::thread::sleep(Duration::from_millis(100));
        };
        let position=|media:&NowPlaying|media.position+if media.playing{media.sampled_at.map_or(0.0,|at|at.elapsed().as_secs_f64())}else{0.0};
        let before=position(&first);
        let lyrics=crate::lyrics::fetch_for_source(&first.source,&first.title,&first.artist,first.duration,first.song_id).expect("未取得歌词");
        assert!(lyrics.synced && !lyrics.lines.is_empty(),"未取得同步歌词");
        std::thread::sleep(Duration::from_secs(4));
        let next=state.lock().unwrap().clone();let after=position(&next);
        assert_eq!((&next.source,&next.title),(&first.source,&first.title),"采样期间切歌，请重新实测");
        assert!(next.playing && after>before+2.0,"播放进度没有推进：{before:.2} -> {after:.2}");
        let cover=next.artwork.expect("播放中封面消失");
        println!("客户端={} 歌曲={} 歌手={} 封面={}x{} 歌词={}行 进度={before:.2}->{after:.2} 歌词行={:?}->{:?}",
            first.source,first.title,first.artist,cover.size[0],cover.size[1],lyrics.lines.len(),lyrics.index_at(before as f32),lyrics.index_at(after as f32));
    }

    #[cfg(windows)]
    #[test]
    #[ignore="验证指定实际客户端的切歌、暂停和恢复，不依赖在线歌词服务"]
    fn live_playback_switch_pause_progress(){
        let expected=std::env::var("DAOZAI_EXPECT_SOURCE").unwrap().to_lowercase();
        let state=shared();spawn_poller(state.clone(),eframe::egui::Context::default());
        let wait=|predicate:&dyn Fn(&NowPlaying)->bool|{
            let start=std::time::Instant::now();loop {
                let media=state.lock().unwrap().clone();
                if media.source.to_lowercase().contains(&expected) && predicate(&media){break media;}
                assert!(start.elapsed()<Duration::from_secs(15),"客户端状态没有就绪：{} / {} playing={}",media.source,media.title,media.playing);
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        let first=wait(&|m|m.is_music() && m.playing && m.artwork.is_some());
        control(Transport::Next,&first.source);
        let next=wait(&|m|m.title!=first.title && m.playing && m.artwork.is_some());
        control(Transport::Toggle,&next.source);let paused=wait(&|m|!m.playing);
        std::thread::sleep(Duration::from_millis(700));assert!(!state.lock().unwrap().playing);
        control(Transport::Toggle,&paused.source);let playing=wait(&|m|m.playing);
        let position=|m:&NowPlaying|m.position+m.sampled_at.map_or(0.0,|at|at.elapsed().as_secs_f64());
        let before=position(&playing);std::thread::sleep(Duration::from_secs(3));let after=state.lock().unwrap().clone();
        assert_eq!(after.title,playing.title);assert!(position(&after)>before+1.0);
        println!("客户端={} 切歌={} -> {} 暂停/恢复通过，进度={} -> {}，曲长={}",first.source,first.title,next.title,playing.position,after.position,after.duration);
    }

    #[cfg(windows)]
    #[test]
    #[ignore="需要当前系统有提供封面的媒体会话"]
    fn live_current_session_artwork(){
        let _apartment=init_winrt();
        let mut cache=ArtworkCache::default();
        let start=std::time::Instant::now();
        let np=loop {
            let np=query(&mut cache).expect("当前没有媒体会话");
            if np.artwork.is_some(){break np;}
            assert!(start.elapsed()<Duration::from_secs(10),"当前媒体会话没有可读封面");
            std::thread::sleep(Duration::from_millis(100));
        };
        println!("当前媒体：{} / {}；来源 {}；类型 {:?}",np.title,np.artist,np.source,np.kind());
        let cover=np.artwork.expect("当前媒体会话没有可读封面");
        println!("系统封面尺寸：{}×{}",cover.size[0],cover.size[1]);
        if let Ok(path)=std::env::var("DAOZAI_ARTWORK_PROBE") {
            let pixels:Vec<u8>=cover.pixels.iter().flat_map(|p|p.to_srgba_unmultiplied()).collect();
            image::save_buffer(path,&pixels,cover.size[0] as u32,cover.size[1] as u32,image::ColorType::Rgba8).unwrap();
        }
    }
}
