//! 歌词：按播放器与歌曲信息匹配 QQ 音乐、网易云及 LRCLIB，同步歌词成功后缓存。

#[derive(Clone, Debug)]
pub struct LyricsLine {
    pub t: f32,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct Lyrics {
    pub lines: Vec<LyricsLine>,
    pub synced: bool,
}

#[derive(Clone,Debug,PartialEq,Eq)]
pub enum FetchError {
    Unavailable(String),
    NotFound,
    InvalidResponse(String),
    IncompleteMetadata,
}
impl FetchError {
    pub fn label(&self)->&'static str {
        match self {
            Self::Unavailable(_)=>"歌词服务暂不可用",
            Self::NotFound=>"未找到匹配歌词",
            Self::InvalidResponse(_)=>"歌词响应异常",
            Self::IncompleteMetadata=>"网页未提供完整歌曲信息",
        }
    }
}

impl Lyrics {
    /// 返回当前时间点应显示的行号。
    pub fn index_at(&self, secs: f32) -> Option<usize> {
        self.lines.iter().rposition(|line|line.t<=secs)
    }
}

fn parse_time(tag: &str) -> Option<f32> {
    // mm:ss.xx 或 mm:ss:xx
    let mut parts = tag.split(':');
    let m: u32 = parts.next()?.trim().parse().ok()?;
    let rest = parts.next()?.trim();
    let seconds=match parts.next() {Some(fraction)=>format!("{rest}.{fraction}"),None=>rest.replace(',', ".")};
    if parts.next().is_some(){return None;}
    let s: f32 = seconds.parse().ok()?;
    if !s.is_finite() || !(0.0..60.0).contains(&s){return None;}
    Some(m as f32 * 60.0 + s)
}

pub fn parse_lrc(raw: &str) -> Lyrics {
    let mut lines = Vec::new();
    for line in raw.lines() {
        let mut times = Vec::new();
        let mut text = line;
        while text.starts_with('[') {
            let start=0;
            let Some(end_rel) = text[start..].find(']') else { break };
            let end = start + end_rel;
            let tag = &text[start + 1..end];
            if let Some(t) = parse_time(tag) {
                times.push(t);
            } else if !times.is_empty() { break; }
            text = &text[end + 1..];
        }
        let body = text.trim();
        if body.is_empty() && times.is_empty() {
            continue;
        }
        for t in times {
            lines.push(LyricsLine { t, text: body.to_string() });
        }
    }
    lines.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    let synced = !lines.is_empty();
    Lyrics { lines, synced }
}

#[test]
fn quality_lyrics_keep_brackets_in_the_actual_text() {
    let parsed=parse_lrc("[00:01.00]唱到[副歌]这里\n[00:02.00][00:03.00]下一句");
    assert_eq!(parsed.lines[0].text,"唱到[副歌]这里");
    assert_eq!(parsed.lines.len(),3);
}

/// 纯文本没有同步时间，保留正文但不伪造逐行时间戳。
pub fn from_plain(raw: &str) -> Lyrics {
    let lines: Vec<LyricsLine> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| LyricsLine { t: 0.0, text: l.trim().to_string() })
        .collect();
    Lyrics { lines, synced: false }
}

#[path="soda_cache.rs"]
mod soda_cache;
#[path="kugou_cache.rs"]
pub(crate) mod kugou_cache;

pub fn is_soda(source:&str)->bool {source=="汽水音乐" || source.to_ascii_lowercase().contains("sodamusic")}

pub(crate) fn read_native(source:&str,track:&str,artist:&str,duration:f64)->Option<Lyrics> {
    if source.to_ascii_lowercase().contains("kugou") {return kugou_cache::read(track,artist,duration);}
    if is_soda(source) {
        let lyrics=parse_krc(&soda_cache::read(track,artist,duration)?);
        return lyrics.synced.then_some(lyrics);
    }
    None
}

/// 返回歌词及是否仍需等待酷狗缓存；等待期间复用已有歌词，不重复访问在线服务。
pub fn fetch_for_display(source:&str,track:&str,artist:&str,duration:f64,song_id:Option<u64>,cached:Option<Lyrics>)->Result<(Lyrics,bool),FetchError>{
    if source.to_ascii_lowercase().contains("kugou"){
        if let Some(lyrics)=kugou_cache::read(track,artist,duration){return Ok((lyrics,false));}
        return cached.map(Ok).unwrap_or_else(||fetch(track,artist,duration)).map(|lyrics|(lyrics,true));
    }
    fetch_for_source(source,track,artist,duration,song_id).map(|lyrics|(lyrics,false))
}

/// 原生缓存优先；未缓存的新歌仍可使用已有在线来源。
pub fn fetch_for_source(source:&str,track:&str,artist:&str,duration:f64,song_id:Option<u64>)->Result<Lyrics,FetchError> {
    if source.to_ascii_lowercase().contains("qqmusic") {
        let common=cache_path(track,artist,duration);
        let path=common.parent().unwrap().join("qqmusic").join(common.file_name().unwrap());
        if let Some(lyrics)=read_cache(&path){return Ok(lyrics);}
        let preferred=fetch_qqmusic("https://c.y.qq.com",track,artist,duration);
        if let Ok(lyrics)=&preferred {if lyrics.synced {write_cache(&path,lyrics);return preferred;}}
        let fallback=fetch(track,artist,duration);
        return match (preferred,fallback) {
            (_,Ok(lyrics)) if lyrics.synced=>Ok(lyrics),
            (Ok(lyrics),_)|(_,Ok(lyrics))=>Ok(lyrics),
            (Err(FetchError::NotFound),Err(error))|(Err(error),_)=>Err(error),
        };
    }
    if source.to_ascii_lowercase().contains("kugou"){
        if let Some(lyrics)=kugou_cache::read(track,artist,duration){return Ok(lyrics);}
    }
    if source.eq_ignore_ascii_case("cloudmusic.exe"){
        if let Some(id)=song_id.filter(|id|*id>0){
            let path=crate::model::config_dir().join("lyrics").join(format!("netease-{id}.lrc"));
            if let Some(lyrics)=read_cache(&path){return Ok(lyrics);}
            if let Ok(lyrics)=fetch_netease_song("https://music.163.com",id){
                if lyrics.synced{write_cache(&path,&lyrics);return Ok(lyrics);}
            }
        }
    }
    if is_soda(source) {
        if let Some(raw)=soda_cache::read(track,artist,duration) {
            let lyrics=parse_krc(&raw);
            if lyrics.synced {return Ok(lyrics);}
        }
    }
    fetch(track,artist,duration)
}

fn parse_krc(raw:&str)->Lyrics {
    let line_tag=regex::Regex::new(r"^\[(\d+),(\d+)\]").unwrap();
    let word_tag=regex::Regex::new(r"<\d+,\d+,\d+>").unwrap();
    let mut timed=Vec::new();
    for line in raw.lines() {
        let Some(c)=line_tag.captures(line) else{continue;};
        let (Ok(start),Ok(length))=(c[1].parse::<u32>(),c[2].parse::<u32>()) else{continue;};
        let Some(end)=start.checked_add(length).filter(|end|*end<=86_400_000) else{continue;};
        let text=word_tag.replace_all(&line[c.get(0).unwrap().end()..],"").trim().to_string();
        timed.push((start,end,text));
    }
    if timed.is_empty(){return parse_lrc(raw);}
    timed.sort_by_key(|line|line.0);
    let mut lines=Vec::new();
    for (i,(start,end,text)) in timed.iter().enumerate() {
        lines.push(LyricsLine{t:*start as f32/1000.0,text:text.clone()});
        // 短暂换气保留上一句，只在至少 3 秒的间奏或尾奏清空歌词。
        if end>start && timed.get(i+1).is_none_or(|next|next.0.saturating_sub(*end)>=3000) {
            lines.push(LyricsLine{t:*end as f32/1000.0,text:String::new()});
        }
    }
    Lyrics{synced:lines.iter().any(|line|!line.text.is_empty()),lines}
}

pub fn fetch(track: &str, artist: &str, duration_secs: f64) -> Result<Lyrics, FetchError> {
    if track.trim().is_empty() || artist.trim().is_empty(){return Err(FetchError::IncompleteMetadata);}
    let path=cache_path(track,artist,duration_secs);
    if let Some(lyrics)=read_cache(&path){return Ok(lyrics);}
    let lyrics=fetch_with_sources("https://music.163.com","https://lrclib.net",track,artist,duration_secs)?;
    if lyrics.synced {write_cache(&path,&lyrics);}
    Ok(lyrics)
}

fn cache_path(track:&str,artist:&str,duration:f64)->std::path::PathBuf {
    let key=format!("{}|{}|{duration:.0}",normalized(track),normalized(artist));
    let hash=key.bytes().fold(14695981039346656037u64,|h,b|(h^b as u64).wrapping_mul(1099511628211));
    crate::model::config_dir().join("lyrics").join(format!("{hash:016x}.lrc"))
}

fn read_cache(path:&std::path::Path)->Option<Lyrics> {
    let lyrics=parse_lrc(&std::fs::read_to_string(path).ok()?);
    (lyrics.synced && lyrics.lines.iter().any(|l|!l.text.is_empty())).then_some(lyrics)
}

fn write_cache(path:&std::path::Path,lyrics:&Lyrics) {
    if let Some(parent)=path.parent(){if std::fs::create_dir_all(parent).is_err(){return;}}
    let raw=lyrics.lines.iter().map(|line|format!("[{:02}:{:06.3}]{}\n",(line.t/60.0) as u32,line.t%60.0,line.text)).collect::<String>();
    let _=crate::model::atomic_write(path,raw.as_bytes());
}

fn fetch_with_sources(netease:&str,lrclib:&str,track:&str,artist:&str,duration:f64)->Result<Lyrics,FetchError> {
    let preferred=fetch_netease(netease,track,artist,duration);
    if preferred.as_ref().is_ok_and(|l|l.synced){return preferred;}
    let fallback=fetch_from(lrclib,track,artist,duration);
    if fallback.as_ref().is_ok_and(|l|l.synced){return fallback;}
    match (preferred,fallback) {
        (Ok(l),_)|(_,Ok(l))=>Ok(l),
        (Err(FetchError::NotFound),Err(e))=>Err(e),
        (Err(e),_)=>Err(e),
    }
}

fn normalized(text:&str)->String {
    text.chars().flat_map(char::to_lowercase).filter(|c|c.is_alphanumeric()).collect()
}

pub(super) fn artist_set(artist:&str)->Vec<String>{
    let mut parts=artist.split(['/', '、', ',', '，', '&', ';', '；']).map(normalized).filter(|s|!s.is_empty()).collect::<Vec<_>>();
    parts.sort();parts.dedup();parts
}

fn netease_candidates(items:&[serde_json::Value],track:&str,artist:&str,duration:f64)->Vec<u64> {
    let title=normalized(track);
    let cleaned=normalized(&clean_title(track));
    let artist_parts=artist_set(artist);
    let mut matches=Vec::new();
    for item in items {
        let Some(id)=item.get("id").and_then(|v|v.as_u64()) else{continue;};
        let name=item.get("name").and_then(|v|v.as_str()).unwrap_or_default();
        let exact=normalized(name)==title;
        if !exact && normalized(&clean_title(name))!=cleaned {continue;}
        let artists=item.get("artists").or_else(||item.get("ar")).and_then(|v|v.as_array());
        let Some(artists)=artists else{continue;};
        let names=artists.iter().filter_map(|a|a.get("name")?.as_str()).collect::<Vec<_>>().join("、");
        if artist_parts.is_empty() || artist_set(&names)!=artist_parts{continue;}
        let candidate_duration=item.get("duration").or_else(||item.get("dt")).and_then(|v|v.as_f64()).unwrap_or_default()/1000.0;
        let difference=if duration>0.0 {(candidate_duration-duration).abs()}else{0.0};
        if duration>0.0 && difference>(duration*0.03).max(10.0){continue;}
        matches.push((id,!exact,difference));
    }
    matches.sort_by(|a,b|a.1.cmp(&b.1).then(a.2.total_cmp(&b.2)));
    matches.into_iter().map(|(id,_,_)|id).take(3).collect()
}

fn fetch_netease(base:&str,track:&str,artist:&str,duration:f64)->Result<Lyrics,FetchError> {
    let url=format!("{base}/api/search/get/web?s={}&type=1&limit=12",urlencode(&format!("{track} {artist}")));
    let result=request_json(&url,base)?;
    if result.get("code").and_then(|v|v.as_i64())!=Some(200){return Err(FetchError::Unavailable("网易云搜索未成功".into()));}
    let items=result.pointer("/result/songs").and_then(|v|v.as_array()).cloned().unwrap_or_default();
    let mut plain=None;
    for id in netease_candidates(&items,track,artist,duration) {
        match fetch_netease_song(base,id){
            Ok(lyrics)if lyrics.synced=>return Ok(lyrics),
            Ok(lyrics)=>plain=Some(lyrics),
            Err(FetchError::NotFound)=>{},
            Err(error)=>return Err(error),
        }
    }
    plain.ok_or(FetchError::NotFound)
}

fn fetch_netease_song(base:&str,id:u64)->Result<Lyrics,FetchError>{
    let result=request_json(&format!("{base}/api/song/lyric?id={id}&lv=-1&kv=-1&tv=-1"),base)?;
    if result.get("code").and_then(|v|v.as_i64())!=Some(200){return Err(FetchError::Unavailable("网易云取词未成功".into()));}
    let raw=result.pointer("/lrc/lyric").and_then(|v|v.as_str()).filter(|s|!s.trim().is_empty()).ok_or(FetchError::NotFound)?;
    let lyrics=parse_lrc(raw);
    if lyrics.synced && lyrics.lines.iter().any(|l|!l.text.is_empty()){Ok(lyrics)}else{Ok(from_plain(raw))}
}

fn qq_candidates(items:&[serde_json::Value],track:&str,artist:&str,duration:f64)->Vec<String> {
    // 统一字段后复用严格匹配，保留歌手、版本与时长约束。
    let candidates:Vec<_>=items.iter().enumerate().map(|(i,item)|serde_json::json!({
        "id":i,"name":item["songname"],"artists":item["singer"],
        "duration":item["interval"].as_f64().unwrap_or_default()*1000.0,
    })).collect();
    netease_candidates(&candidates,track,artist,duration).into_iter().filter_map(|i|
        items[i as usize]["songmid"].as_str().filter(|mid|!mid.is_empty() && mid.bytes().all(|b|b.is_ascii_alphanumeric())).map(str::to_owned)
    ).collect()
}

fn fetch_qqmusic(base:&str,track:&str,artist:&str,duration:f64)->Result<Lyrics,FetchError> {
    if track.trim().is_empty() || artist.trim().is_empty(){return Err(FetchError::IncompleteMetadata);}
    let query=urlencode(&format!("{track} {artist}"));
    let result=request_json_with_retry(&format!("{base}/soso/fcgi-bin/client_search_cp?w={query}&format=json&p=1&n=12"),"https://y.qq.com/",true)?;
    if result["code"].as_i64()!=Some(0){return Err(FetchError::Unavailable("QQ 音乐搜索未成功".into()));}
    let items=result.pointer("/data/song/list").and_then(|v|v.as_array()).ok_or_else(||FetchError::InvalidResponse("QQ 音乐未返回歌曲列表".into()))?;
    let mut plain=None;
    for mid in qq_candidates(items,track,artist,duration) {
        let result=request_json_with_retry(&format!("{base}/lyric/fcgi-bin/fcg_query_lyric_new.fcg?songmid={mid}&format=json&nobase64=1"),"https://y.qq.com/",true)?;
        if result["code"].as_i64()!=Some(0){return Err(FetchError::Unavailable("QQ 音乐取词未成功".into()));}
        if let Some(raw)=result["lyric"].as_str().filter(|raw|!raw.trim().is_empty()) {
            let lyrics=parse_lrc(raw);
            if lyrics.synced && lyrics.lines.iter().any(|line|!line.text.is_empty()){return Ok(lyrics);}
            plain=Some(from_plain(raw));
        }
    }
    plain.ok_or(FetchError::NotFound)
}

fn request_json(url:&str,referer:&str)->Result<serde_json::Value,FetchError> {
    request_json_with_retry(url,referer,false)
}

fn request_json_with_retry(url:&str,referer:&str,retry:bool)->Result<serde_json::Value,FetchError> {
    let request=ureq::get(url)
        .set("User-Agent",concat!("Daozai/",env!("CARGO_PKG_VERSION")," (Windows)"))
        .set("Referer",referer)
        .set("Accept","application/json")
        .timeout(std::time::Duration::from_secs(5));
    let mut result=request.clone().call();
    // QQ 的偶发连接超时、网关错误只重试一次；拒绝访问和无歌词不重复请求。
    if retry && result.as_ref().is_err_and(|error|matches!(error,ureq::Error::Transport(_)|ureq::Error::Status(408|500|502|503|504,_))) {
        std::thread::sleep(std::time::Duration::from_millis(200));
        result=request.call();
    }
    let response=result.map_err(|e|FetchError::Unavailable(e.to_string()))?;
    let text=response.into_string().map_err(|e|FetchError::InvalidResponse(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e|FetchError::InvalidResponse(e.to_string()))
}

fn clean_title(track:&str)->String {
    let suffix=regex::Regex::new(r"(?i)\s*[\(\[（【]\s*(?:official(?:\s+music)?\s+(?:video|audio)|lyrics?|lyric\s+video)\s*[\)\]）】]").unwrap();
    suffix.replace_all(track,"").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fetch_from(base:&str,track:&str,artist:&str,duration_secs:f64)->Result<Lyrics,FetchError> {
    if track.trim().is_empty() {
        return Err(FetchError::NotFound);
    }
    let cleaned=clean_title(track);
    let mut titles=vec![track.trim()];
    if !cleaned.is_empty() && cleaned!=track.trim(){titles.push(&cleaned);}
    for title in titles {
        let items=search(base,title,artist)?;
        let artists=artist_set(artist);
        let items=items.into_iter().filter(|item|{
            item.get("trackName").and_then(|v|v.as_str()).is_some_and(|name|normalized(&clean_title(name))==normalized(&clean_title(title)))
                && !artists.is_empty() && item.get("artistName").and_then(|v|v.as_str()).is_some_and(|name|artist_set(name)==artists)
        }).collect::<Vec<_>>();
        if let Ok(lyrics)=lyrics_from_items(&items,duration_secs){return Ok(lyrics);}
    }
    Err(FetchError::NotFound)
}

fn search(base:&str,track:&str,artist:&str)->Result<Vec<serde_json::Value>,FetchError> {
    let url = format!(
        "{base}/api/search?track_name={}&artist_name={}",
        urlencode(track),
        urlencode(artist)
    );
    let arr=request_json(&url,base)?;
    arr.as_array().cloned().ok_or_else(||FetchError::InvalidResponse("预期歌词列表".into()))
}

fn lyrics_from_items(items:&[serde_json::Value],duration_secs:f64)->Result<Lyrics,FetchError> {
    let mut synced:Option<(Lyrics,f64)>=None;
    let mut plain:Option<(Lyrics,f64)>=None;
    for it in items {
        let d = it.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let score=if duration_secs>0.0{(d-duration_secs).abs()}else{0.0};
        if duration_secs>0.0 && d>0.0 && score>(duration_secs*0.03).max(10.0){continue;}
        if let Some(raw)=it.get("syncedLyrics").and_then(|v|v.as_str()) {
            let parsed=parse_lrc(raw);
            if !parsed.lines.is_empty() && synced.as_ref().is_none_or(|(_,s)|score<*s){synced=Some((parsed,score));}
        }
        if let Some(raw)=it.get("plainLyrics").and_then(|v|v.as_str()).filter(|s|!s.trim().is_empty()) {
            if plain.as_ref().is_none_or(|(_,s)|score<*s){plain=Some((from_plain(raw),score));}
        }
    }
    synced.or(plain).map(|(l,_)|l).ok_or(FetchError::NotFound)
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn qq_matching_rejects_wrong_artist_version_duration_and_invalid_id() {
        let items=serde_json::json!([
            {"songmid":"wrongartist","songname":"Track","singer":[{"name":"Cover"}],"interval":215},
            {"songmid":"live","songname":"Track (Live)","singer":[{"name":"Artist"}],"interval":215},
            {"songmid":"long","songname":"Track","singer":[{"name":"Artist"}],"interval":300},
            {"songmid":"bad&id=1","songname":"Track","singer":[{"name":"Artist"}],"interval":215},
            {"songmid":"correct","songname":"Track","singer":[{"name":"Artist"}],"interval":215}
        ]);
        assert_eq!(qq_candidates(items.as_array().unwrap(),"Track","Artist",215.0),vec!["correct"]);
    }

    #[test]
    fn qq_fetch_handles_synced_missing_and_unavailable_lyrics() {
        let search=serde_json::json!({"code":0,"data":{"song":{"list":[{"songmid":"testMID","songname":"Track","singer":[{"name":"Artist"}],"interval":215}]}}}).to_string();
        for (code,raw,expected) in [(0,"[00:01.00]第一句\n[00:05.00]第二句",0),(0,"",1),(-1,"",2)] {
            let lyric=serde_json::json!({"code":code,"lyric":raw}).to_string();
            let (base,worker)=serve(vec![("200 OK",search.clone()),("200 OK",lyric)]);
            let result=fetch_qqmusic(&base,"Track","Artist",215.0);
            match expected {0=>assert!(result.unwrap().synced),1=>assert_eq!(result.unwrap_err(),FetchError::NotFound),_=>assert!(matches!(result,Err(FetchError::Unavailable(_))))}
            worker.join().unwrap();
        }
    }

    #[test]
    #[ignore="按需验证用户反馈的 QQ 音乐歌曲，只输出同步状态和行数"]
    fn live_qq_reported_track() {
        let lyrics=fetch_for_source("QQMusic.exe","兰香如故","叶炫清",215.0,None).unwrap();
        assert!(lyrics.synced && lyrics.lines.len()>10);
        assert!(lyrics.index_at(105.0)>lyrics.index_at(52.0));
        println!("QQ 音乐：同步歌词 {} 行，时间轴正常",lyrics.lines.len());
    }

    #[test]
    #[ignore="绕过歌词缓存，验证 QQ 在线搜索及取词"]
    fn live_qq_uncached_tracks() {
        for (track,artist,duration) in [("Gotcha","鹭卓",153.0),("不死鸟","霍尊",265.772),("兰香如故","叶炫清",215.0)] {
            let start=std::time::Instant::now();
            let result=fetch_qqmusic("https://c.y.qq.com",track,artist,duration);
            println!("{track}: {:.2}s {:?}",start.elapsed().as_secs_f64(),result.as_ref().map(|lyrics|(lyrics.synced,lyrics.lines.len())));
            assert!(result.unwrap().synced);
        }
    }

    #[test]
    fn qq_recovers_from_a_transient_search_failure() {
        let search=serde_json::json!({"code":0,"data":{"song":{"list":[{"songmid":"testMID","songname":"Track","singer":[{"name":"Artist"}],"interval":215}]}}}).to_string();
        let lyric=serde_json::json!({"code":0,"lyric":"[00:01.00]第一句\n[00:05.00]第二句"}).to_string();
        let (base,worker)=serve(vec![("502 Bad Gateway",String::new()),("200 OK",search),("200 OK",lyric)]);
        let result=fetch_qqmusic(&base,"Track","Artist",215.0).unwrap();
        assert!(result.synced);assert_eq!(result.lines.len(),2);
        worker.join().unwrap();
    }

    #[test]
    fn qq_timeout_retries_once_and_permanent_errors_stay_errors() {
        use std::{io::{Read,Write},net::TcpListener,time::Duration};
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let url=format!("http://{}",listener.local_addr().unwrap());
        let worker=std::thread::spawn(move||{
            let (mut stalled,_)=listener.accept().unwrap();
            let mut buffer=[0;4096];stalled.read(&mut buffer).unwrap();
            // 保持第一条连接不响应，第二次请求才返回，真实覆盖五秒读取超时。
            let (mut recovered,_)=listener.accept().unwrap();
            recovered.read(&mut buffer).unwrap();
            write!(recovered,"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
        });
        let started=std::time::Instant::now();
        assert!(request_json_with_retry(&url,&url,true).is_ok());
        assert!(started.elapsed()<Duration::from_secs(8));worker.join().unwrap();
        for responses in [vec![("503 Service Unavailable",String::new());2],vec![("403 Forbidden",String::new())]] {
            let (base,worker)=serve(responses);
            assert!(matches!(request_json_with_retry(&base,&base,true),Err(FetchError::Unavailable(_))));
            worker.join().unwrap();
        }
    }

    #[test]
    #[ignore="需要本机酷狗已缓存这两首歌的原生歌词，用于复现跨来源时间戳偏差"]
    fn live_kugou_uses_native_timestamps(){
        for (title,artist,text,expected) in [("一生有你","水木年华","因为梦见你离开",22.991f32),("口是心非","张雨生","口是心非",65.434f32)]{
            let lyrics=super::fetch_for_source("kugou",title,artist,0.0,None).unwrap();
            let line=lyrics.lines.iter().find(|line|line.text==text).unwrap();
            assert!((line.t-expected).abs()<0.002,"{title} 应跟随酷狗原生时间戳：实际 {}，原生 {expected}",line.t);
            println!("{title} 原生歌词时间戳校验通过：{expected:.3}s");
        }
    }
    use super::*;

    #[test]
    #[ignore="需要本机酷狗已缓存《谁伴我闯荡》，校验实际短停顿"]
    fn live_kugou_short_pause_stays_on_previous_line() {
        let (l,waiting)=fetch_for_display("kugou","谁伴我闯荡","BEYOND",0.0,None,None).unwrap();
        assert!(!waiting,"应使用酷狗原生歌词");
        for t in [28.434,29.164,29.863] {
            assert_eq!(l.lines[l.index_at(t).unwrap()].text,"前面是那方谁伴我闯荡");
        }
        assert_eq!(l.lines[l.index_at(29.864).unwrap()].text,"沿路没有指引若我走上");
    }

    #[test]
    fn krc_short_gaps_hold_previous_line_without_changing_timing() {
        for gap in [20,390,1037,1984,2126,2999] {
            let next=3000+gap;
            let l=parse_krc(&format!("[1000,2000]<0,2000,0>上一句\n[{next},2000]<0,2000,0>下一句"));
            let next=next as f32/1000.0;
            assert!(l.index_at(0.9).is_none());
            for t in [3.0,3.0+gap as f32/2000.0,next-0.001] {
                assert_eq!(l.lines[l.index_at(t).unwrap()].text,"上一句","{gap} 毫秒的行间空隙应保留上一句");
            }
            assert_eq!(l.lines[l.index_at(next).unwrap()].text,"下一句");
            assert_eq!(l.lines[l.index_at(next+2.0).unwrap()].text,"");
        }
        for gap in [3000,8000] {
            let l=parse_krc(&format!("[1000,2000]上一句\n[{},2000]下一句",3000+gap));
            assert_eq!(l.lines[l.index_at(3.0).unwrap()].text,"");
        }
    }

    #[test]
    fn soda_krc_preserves_real_timing_and_instrumental_gaps() {
        let l=parse_krc("[25378,4523]<0,202,0>孤<202,201,0>单\n[34938,5563]<0,202,0>身<202,201,0>边");
        assert!(l.synced);
        assert!((l.lines[0].t-25.378).abs()<0.001);
        assert_eq!(l.lines[0].text,"孤单");
        assert_eq!(l.lines[l.index_at(30.0).unwrap()].text,"");
        assert_eq!(l.lines[l.index_at(35.0).unwrap()].text,"身边");
        assert!(!parse_krc("[bad,10]<0,10,0>错").synced);
        assert!(!parse_krc("纯文本歌词").synced);
        assert_eq!(parse_krc("[00:01.20]正文").lines[0].text,"正文");
    }


    #[test]
    #[ignore="需要实际访问外部歌词服务"]
    fn live_reference_track() {
        let result=fetch("Never Gonna Give You Up","Rick Astley",213.0);
        println!("reference: {:?}",result.as_ref().map(|l|(l.synced,l.lines.len())));
        assert!(result.is_ok());
    }

    #[test]
    #[ignore="需要实际访问外部歌词服务"]
    fn live_web_player_track() {
        let result=fetch("スパークル (movie ver.)","RADWIMPS",534.0);
        println!("web-player: {:?}",result.as_ref().map(|l|(l.synced,l.lines.len())));
        assert!(result.unwrap().synced);
    }

    #[test]
    #[ignore="需要实际访问外部歌词服务"]
    fn live_chinese_tracks() {
        for (title,artist,duration) in [("稀客","杨千嬅",190.0),("阿拉斯加海湾","蓝心羽",241.0)] {
            let lyrics=fetch_with_sources("https://music.163.com","https://lrclib.net",title,artist,duration).unwrap();
            assert!(lyrics.synced);
            let first=lyrics.index_at(30.0).unwrap();let later=lyrics.index_at(90.0).unwrap();
            println!("{title} / {artist}: {} 条时间戳，30 秒行号 {first}，90 秒行号 {later}",lyrics.lines.len());
            assert!(later>first,"歌词应随播放进度前进");
        }
    }

    #[test]
    fn matching_rejects_covers_wrong_titles_and_other_versions() {
        let items=serde_json::json!([
            {"id":1,"name":"Track","artists":[{"name":"Cover Artist"}],"duration":120000},
            {"id":2,"name":"Different Track","artists":[{"name":"Artist"}],"duration":120000},
            {"id":3,"name":"Track (Live)","artists":[{"name":"Artist"}],"duration":120000},
            {"id":4,"name":"Track","artists":[{"name":"Artist"}],"duration":200000},
            {"id":5,"name":"Track","artists":[{"name":"Artist"}],"duration":121000},
            {"id":6,"name":"Track","ar":[{"name":"Artist"}],"dt":122000}
        ]);
        assert_eq!(netease_candidates(items.as_array().unwrap(),"Track","Artist",120.0),vec![5,6]);
    }

    fn serve(responses:Vec<(&'static str,String)>)->(String,std::thread::JoinHandle<()>) {
        use std::{io::{Read,Write},net::TcpListener};
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let base=format!("http://{}",listener.local_addr().unwrap());
        let worker=std::thread::spawn(move||for (status,body) in responses {
            let (mut stream,_)=listener.accept().unwrap();
            let mut buf=[0;4096];let _=stream.read(&mut buf).unwrap();
            write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        });
        (base,worker)
    }

    #[test]
    fn fallback_checks_identity_even_when_wrong_recording_has_exact_duration(){
        let candidates=serde_json::json!([
            {"id":1,"name":"合唱","artists":[{"name":"甲"},{"name":"丙"}],"duration":120000},
            {"id":2,"name":"合唱","artists":[{"name":"乙"},{"name":"甲"}],"duration":120000}
        ]);
        assert_eq!(netease_candidates(candidates.as_array().unwrap(),"合唱","甲/乙",120.0),vec![2]);
        for (name,artist) in [("Other Track","Artist"),("Track (Live)","Artist"),("Track (movie ver.)","Artist"),("Track","Cover Artist"),("Track","")] {
            let rows=serde_json::json!([{"trackName":name,"artistName":artist,"duration":120,"syncedLyrics":"[00:01.00]不应采用"}]);
            let (base,worker)=serve(vec![("200 OK",rows.to_string())]);
            assert_eq!(fetch_from(&base,"Track","Artist",120.0).unwrap_err(),FetchError::NotFound);worker.join().unwrap();
        }
        let rows=serde_json::json!([{"trackName":"Track","artistName":"乙、甲","duration":120,"syncedLyrics":"[00:01.00]正确歌词"}]);
        let (base,worker)=serve(vec![("200 OK",rows.to_string())]);
        assert_eq!(fetch_from(&base,"Track","甲/乙",120.0).unwrap().lines[0].text,"正确歌词");worker.join().unwrap();
    }

    #[test]
    fn online_resolver_uses_a_working_source_when_the_other_is_down() {
        let search=serde_json::json!({"code":200,"result":{"songs":[{"id":5,"name":"Track","artists":[{"name":"Artist"}],"duration":120000}]}}).to_string();
        let lyric=serde_json::json!({"code":200,"lrc":{"lyric":"[00:01.00]first\n[00:05.00]second"}}).to_string();
        let (netease,worker)=serve(vec![("200 OK",search),("200 OK",lyric)]);
        // 不可达的备用地址不应影响主源正常返回。
        assert!(fetch_with_sources(&netease,"http://127.0.0.1:1","Track","Artist",120.0).unwrap().synced);
        worker.join().unwrap();
        let (netease,first)=serve(vec![("503 Service Unavailable",String::new())]);
        let (lrclib,second)=serve(vec![("200 OK",r#"[{"trackName":"Track","artistName":"Artist","duration":120,"syncedLyrics":"[00:01.00]first\n[00:05.00]second"}]"#.into())]);
        assert!(fetch_with_sources(&netease,&lrclib,"Track","Artist",120.0).unwrap().synced);
        first.join().unwrap();second.join().unwrap();
    }

    #[test]
    fn netease_song_id_fetches_lyrics_without_a_search_request(){
        let lyric=serde_json::json!({"code":200,"lrc":{"lyric":"[00:01.00]第一句\n[00:05.00]第二句"}}).to_string();
        let (base,worker)=serve(vec![("200 OK",lyric)]);
        let lyrics=fetch_netease_song(&base,123).unwrap();
        assert!(lyrics.synced);assert_eq!(lyrics.lines.len(),2);worker.join().unwrap();
    }

    #[test]
    fn cached_lyrics_preserve_timing_and_invalid_caches_are_ignored() {
        let path=std::env::temp_dir().join(format!("daozai-lrc-cache-{}-{:?}.lrc",std::process::id(),std::thread::current().id()));
        let expected=parse_lrc("[00:01.234]first\n[00:12.567]second\n[01:03.891]third");
        write_cache(&path,&expected);
        let cached=read_cache(&path).unwrap();
        assert_eq!(cached.lines.len(),expected.lines.len());
        for (a,b) in cached.lines.iter().zip(&expected.lines) {
            assert_eq!(a.text,b.text);assert!((a.t-b.t).abs()<0.002);
        }
        std::fs::write(&path,"broken cache").unwrap();
        assert!(read_cache(&path).is_none());
        let _=std::fs::remove_file(path);
    }

    #[test]
    fn parser_keeps_fractional_colon_times_and_rejects_invalid_timing() {
        let lyrics=parse_lrc("[00:12:35]first\n[00:NaN]invalid\n[00:61.00]invalid\n[00:12:35:10]invalid");
        assert_eq!(lyrics.lines.len(),1);assert!((lyrics.lines[0].t-12.35).abs()<0.001);
    }

    #[test]
    fn parses_and_sorts_lrc() {
        let raw = "[00:12.50]second\n[00:01.00]first\n[00:20:00]third";
        let l = parse_lrc(raw);
        assert_eq!(l.lines.len(), 3);
        assert_eq!(l.lines[0].text, "first");
        assert_eq!(l.lines[1].text, "second");
        assert!((l.lines[2].t - 20.0).abs() < 1e-3);
        assert!(l.synced);
    }

    #[test]
    fn index_at_tracks_position() {
        let l = parse_lrc("[00:01.00]a\n[00:05.00]b\n[00:09.00]c");
        assert_eq!(l.index_at(0.0), None);
        assert_eq!(l.index_at(6.0), Some(1));
        assert_eq!(l.index_at(100.0), Some(2));
    }

    #[test]
    fn plain_lyrics_have_no_invented_timing() {
        let l = from_plain("one\ntwo\n\nthree");
        assert_eq!(l.lines.len(), 3);
        assert!(!l.synced);
        assert!(l.lines.iter().all(|line|line.t==0.0));
    }

    #[test]
    fn title_fallback_keeps_meaningful_versions() {
        assert_eq!(clean_title("スパークル (movie ver.)"),"スパークル (movie ver.)");
        assert_eq!(clean_title("Track (Official Music Video)"),"Track");
        assert_eq!(clean_title("Track (Live)"),"Track (Live)");
    }

    #[test]
    fn matching_prefers_synced_lyrics_and_rejects_other_versions() {
        let items=serde_json::json!([
            {"duration":534,"plainLyrics":"plain"},
            {"duration":535,"syncedLyrics":"[00:01.00]first\n[00:05.00]second"},
            {"duration":200,"syncedLyrics":"[00:01.00]wrong version"}
        ]);
        let result=lyrics_from_items(items.as_array().unwrap(),534.0).unwrap();
        assert!(result.synced);assert_eq!(result.lines[0].text,"first");
        assert_eq!(lyrics_from_items(&[items[2].clone()],534.0).unwrap_err(),FetchError::NotFound);
    }

    #[test]
    fn http_search_and_network_errors_are_distinct_from_missing_lyrics() {
        use std::{io::{Read,Write},net::TcpListener};
        for (status,body,expected) in [("200 OK",r#"[{"trackName":"Track Name","artistName":"Artist","duration":120,"syncedLyrics":"[00:01.00]first"}]"#,0),("200 OK","[]",1),("502 Bad Gateway","",2)] {
            let listener=TcpListener::bind("127.0.0.1:0").unwrap();
            let base=format!("http://{}",listener.local_addr().unwrap());
            let worker=std::thread::spawn(move||{
                let (mut stream,_)=listener.accept().unwrap();
                let mut buf=[0;4096];let n=stream.read(&mut buf).unwrap();
                let request=String::from_utf8_lossy(&buf[..n]);
                assert!(request.contains("track_name=Track%20Name&artist_name=Artist"));
                write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            });
            let result=fetch_from(&base,"Track Name","Artist",120.0);
            worker.join().unwrap();
            match expected {0=>assert!(result.unwrap().synced),1=>assert_eq!(result.unwrap_err(),FetchError::NotFound),_=>assert!(matches!(result,Err(FetchError::Unavailable(_))))}
        }
    }

    #[test]
    fn urlencode_escapes_spaces_and_symbols() {
        assert_eq!(urlencode("a b&c"), "a%20b%26c");
    }
}
