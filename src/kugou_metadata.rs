//! 系统会话缺少曲长时，只读酷狗本机的歌曲元数据。
use std::{ffi::c_void,time::{Duration,Instant}};

fn track(raw:&[u8],filename:&str)->Option<(String,f64)>{
    let value=serde_json::Deserializer::from_slice(raw).into_iter::<serde_json::Value>().next()?.ok()?;
    if value.get("type")?.as_str()? != "song" || value.get("filename")?.as_str()? != filename{return None;}
    let hash=value.get("hash")?.as_str()?;
    if hash.len()!=32 || !hash.bytes().all(|b|b.is_ascii_hexdigit()){return None;}
    let duration=value.get("duration")?;
    let duration=duration.as_str().and_then(|v|v.parse::<f64>().ok()).or_else(||duration.as_f64())?/1000.0;
    if !duration.is_finite() || duration<=0.0 || duration>=86400.0{return None;}
    Some((filename.split_once(" - ")?.0.to_owned(),duration))
}

fn scan(bytes:&[u8],filename:&str)->Option<(String,f64)>{
    const MARKER:&[u8]=b"\"duration\":";
    for (offset,_) in bytes.windows(MARKER.len()).enumerate().filter(|(_,b)|*b==MARKER) {
        // 只保留歌曲字段；不读取或记录周围的请求、账号或日志内容。
        let end=(offset+8192).min(bytes.len());
        let mut raw=Vec::with_capacity(end-offset+1);raw.push(b'{');raw.extend_from_slice(&bytes[offset..end]);
        if let Some(metadata)=track(&raw,filename){return Some(metadata);}
    }
    None
}

type Handle=*mut c_void;
#[repr(C)]
#[derive(Default)]
struct Region {base:usize,allocation:usize,allocation_protect:u32,size:usize,state:u32,protect:u32,kind:u32}
#[link(name="kernel32")]
extern "system" {
    fn OpenProcess(access:u32,inherit:i32,pid:u32)->Handle;
    fn CloseHandle(handle:Handle)->i32;
    fn ReadProcessMemory(handle:Handle,address:usize,buffer:*mut c_void,size:usize,read:*mut usize)->i32;
    fn VirtualQueryEx(handle:Handle,address:usize,region:*mut Region,size:usize)->usize;
    fn K32GetModuleBaseNameW(handle:Handle,module:usize,name:*mut u16,size:u32)->u32;
}
#[link(name="user32")]
extern "system" {
    fn EnumWindows(callback:unsafe extern "system" fn(Handle,isize)->i32,param:isize)->i32;
    fn GetWindowTextW(window:Handle,text:*mut u16,size:i32)->i32;
    fn GetWindowThreadProcessId(window:Handle,pid:*mut u32)->u32;
}
struct Process(Handle);
impl Drop for Process {fn drop(&mut self){unsafe{CloseHandle(self.0);}}}

fn window_filename(text:&str,title:&str,artist:&str)->Option<String>{
    let filename=text.trim().strip_suffix(" - 酷狗音乐")?;
    let singer=filename.strip_suffix(&format!(" - {title}"))?;
    if singer.is_empty(){return None;}
    if !artist.trim().is_empty() && artist!=singer {
        let separators=['、','/','&',',','，',';','；'];
        let singers:Vec<_>=singer.split(separators).map(str::trim).filter(|s|!s.is_empty()).collect();
        let reported:Vec<_>=artist.split(separators).map(str::trim).filter(|s|!s.is_empty()).collect();
        if reported.is_empty() || !reported.iter().all(|s|singers.contains(s)){return None;}
    }
    Some(filename.to_owned())
}

fn current(title:&str,artist:&str)->Option<(u32,String)>{
    struct Search<'a>{title:&'a str,artist:&'a str,result:Option<(u32,String)>}
    unsafe extern "system" fn visit(window:Handle,param:isize)->i32{
        let search=&mut *(param as *mut Search<'_>);
        let mut text=[0u16;1024];let len=GetWindowTextW(window,text.as_mut_ptr(),1024);
        if len<=0{return 1;}
        let text=String::from_utf16_lossy(&text[..len as usize]);
        let Some(filename)=window_filename(&text,search.title,search.artist)else{return 1;};
        let mut pid=0;GetWindowThreadProcessId(window,&mut pid);
        let process=Process(OpenProcess(0x410,0,pid));if process.0.is_null(){return 1;}
        let mut name=[0u16;260];let len=K32GetModuleBaseNameW(process.0,0,name.as_mut_ptr(),260);
        if !String::from_utf16_lossy(&name[..len as usize]).eq_ignore_ascii_case("KuGou.exe"){return 1;}
        search.result=Some((pid,filename.to_owned()));0
    }
    let mut search=Search{title,artist,result:None};
    unsafe{EnumWindows(visit,&mut search as *mut Search<'_> as isize);}
    search.result
}

fn read(pid:u32,filename:&str)->Option<(String,f64)>{
    let process=Process(unsafe{OpenProcess(0x410,0,pid)});if process.0.is_null(){return None;}
    let start=Instant::now();let mut address=0;let mut total=0;
    while total<256*1024*1024 && start.elapsed()<Duration::from_secs(2) {
        let mut region=Region::default();
        if unsafe{VirtualQueryEx(process.0,address,&mut region,std::mem::size_of::<Region>())}==0{break;}
        address=region.base.checked_add(region.size)?;
        if region.state!=0x1000 || region.kind!=0x20000 || region.protect!=4 {continue;}
        let mut offset=0;
        while offset<region.size && total<256*1024*1024 && start.elapsed()<Duration::from_secs(2) {
            let size=(region.size-offset).min(1024*1024);let mut bytes=vec![0;size];let mut count=0;
            if unsafe{ReadProcessMemory(process.0,region.base+offset,bytes.as_mut_ptr().cast(),size,&mut count)}!=0 {
                if let Some(metadata)=scan(&bytes[..count],filename){return Some(metadata);}
            }
            total+=size;
            if size<=8192{break;}offset+=size-8192;
        }
    }
    None
}

#[derive(Default)]
pub struct Cache {key:Option<(u32,String)>,value:Option<(String,f64)>,checked:Option<Instant>,seen:Option<Instant>}
impl Cache {
    fn recent_key(&self,title:&str,artist:&str)->Option<(u32,String)>{
        if self.seen?.elapsed()>Duration::from_secs(2){return None;}
        let key=self.key.as_ref()?;
        window_filename(&format!("{} - 酷狗音乐",key.1),title,artist)?;
        Some(key.clone())
    }
    pub fn get(&mut self,title:&str,artist:&str)->Option<(String,f64)>{
        // 循环重载时窗口标题会短暂消失；同一首歌最多沿用两秒已核对的身份。
        let key=if let Some(key)=current(title,artist){self.seen=Some(Instant::now());key}else{self.recent_key(title,artist)?};
        if self.key.as_ref()!=Some(&key){self.key=Some(key.clone());self.value=None;self.checked=None;}
        if self.value.is_none() && self.checked.is_none_or(|at|at.elapsed()>=Duration::from_secs(5)) {
            self.checked=Some(Instant::now());self.value=read(key.0,&key.1);
        }
        if current(title,artist).is_some_and(|next|next!=key){return None;}
        // 曲长尚未就绪也能使用已核对的窗口署名，不能让合唱退化为单个歌手。
        let singer=key.1.strip_suffix(&format!(" - {title}"))?.to_owned();
        Some((singer,self.value.as_ref().map_or(0.0,|(_,duration)|*duration)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeat_title_gap_keeps_only_recent_matching_identity(){
        let mut cache=Cache{key:Some((1,"添儿呗、刘瑾屹 - 溺云歌".into())),seen:Some(Instant::now()),..Default::default()};
        assert!(cache.recent_key("溺云歌","添儿呗").is_some());
        assert!(cache.recent_key("溺云歌 (Live)","添儿呗").is_none());
        assert!(cache.recent_key("溺云歌","其他歌手").is_none());
        cache.seen=Some(Instant::now()-Duration::from_secs(3));assert!(cache.recent_key("溺云歌","添儿呗").is_none());
    }
    #[test]
    fn repeat_with_primary_artist_keeps_the_same_native_identity(){
        let window="添儿呗、刘瑾屹 - 溺云歌 - 酷狗音乐 ";
        for artist in ["添儿呗、刘瑾屹","添儿呗","", "刘瑾屹/添儿呗"] {
            assert_eq!(window_filename(window,"溺云歌",artist).as_deref(),Some("添儿呗、刘瑾屹 - 溺云歌"),"循环播放时系统歌手字段变化不能丢失本机歌曲身份");
        }
        for artist in ["添儿","另一歌手","添儿呗、另一歌手"] {assert!(window_filename(window,"溺云歌",artist).is_none());}
        assert!(window_filename(window,"溺云歌 (Live)","添儿呗").is_none());
    }
    #[test]
    fn scan_ignores_unrelated_and_truncated_objects(){
        let raw=br#"junk{"duration":"1000","filename":"Other - Song","type":"song","hash":"0123456789abcdef0123456789abcdef"}garbage{"duration":"232000","filename":"cici_ - Song (Live)","type":"song","hash":"8c4c4839a9aa418f528ebb7a8ce42afe"}trailing"#;
        assert_eq!(scan(raw,"cici_ - Song (Live)"),Some(("cici_".into(),232.0)));
        assert!(scan(&raw[..raw.len()-20],"cici_ - Song (Live)").is_none());
    }
    #[test]
    fn exact_track_duration_rejects_other_versions_and_invalid_values(){
        let raw=br#"{"duration":"232000","filename":"cici_ - Song (Live)","type":"song","hash":"8c4c4839a9aa418f528ebb7a8ce42afe","progress":"560000"}"#;
        assert_eq!(track(raw,"cici_ - Song (Live)"),Some(("cici_".into(),232.0)));
        assert!(track(raw,"cici_ - Song").is_none());
        assert!(track(raw,"Other - Song (Live)").is_none());
        for invalid in ["0","-1000","NaN","86400000"] {
            let changed=String::from_utf8(raw.to_vec()).unwrap().replace("232000",invalid);
            assert!(track(changed.as_bytes(),"cici_ - Song (Live)").is_none());
        }
        assert!(track(b"broken","cici_ - Song (Live)").is_none());
    }
}
