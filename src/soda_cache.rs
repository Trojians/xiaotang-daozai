//! 只读解析汽水 3.7 媒体缓存中的原生歌词；不读取登录配置或音频。
use serde_json::{Value,Map};
use std::collections::{HashMap,HashSet};

fn le(data:&[u8],offset:usize,n:usize)->Option<usize>{
    Some(data.get(offset..offset.checked_add(n)?)?.iter().enumerate().fold(0,|v,(i,b)|v|((*b as usize)<<(8*i))))
}

/// 仅遍历当前已提交快照的主数据库，忽略空闲页及旧事务内容。
fn entries(data:&[u8])->Option<Vec<&[u8]>> {
    if le(data,24,4)?!=0xbeefc0de || le(data,28,4)?!=2{return None;}
    let size=le(data,48,4)?;
    if !size.is_power_of_two() || !(512..=65536).contains(&size){return None;}
    if le(data,size+24,4)?!=0xbeefc0de || le(data,size+28,4)?!=2{return None;}
    let meta=if le(data,152,8)?>le(data,size+152,8)?{0}else{size};
    let mut pending=vec![le(data,meta+136,8)?];let mut visited=HashSet::new();let mut result=Vec::new();
    while let Some(page)=pending.pop(){
        if !visited.insert(page) || visited.len()>4096{return None;}
        let base=page.checked_mul(size)?;let bytes=data.get(base..base.checked_add(size)?)?;
        if le(bytes,0,8)?!=page{return None;}
        let flags=le(bytes,18,2)?;if flags!=1 && flags!=2{return None;}
        let lower=le(bytes,20,2)?;if lower%2!=0 || lower>size-24{return None;}
        for i in 0..lower/2 {
            let node=24+le(bytes,24+i*2,2)?;
            let len=le(bytes,node,4)?;let flags_node=le(bytes,node+4,2)?;let key=le(bytes,node+6,2)?;
            if flags==1 {pending.push(len | (flags_node<<32));continue;}
            if flags_node & 6!=0 {continue;}
            let start=node.checked_add(8+key)?;
            let value=if flags_node==1 {
                let page=le(bytes,start,8)?;let start=page.checked_mul(size)?;
                if le(data,start,8)?!=page || le(data,start+18,2)?!=4{return None;}
                let count=le(data,start+20,4)?;
                if len>count.checked_mul(size)?.checked_sub(24)?{return None;}
                data.get(start+24..(start+24).checked_add(len)?)?
            }else if flags_node==0{bytes.get(start..start.checked_add(len)?)?}else{return None;};
            if len>2*1024*1024{return None;}
            result.push(value);
        }
    }
    Some(result)
}

// 汽水使用 MessagePack 的内联 record 扩展；记录表仅在单个值内有效。
struct Decoder<'a>{data:&'a[u8],pos:usize,records:HashMap<i32,Vec<String>>,two:HashSet<i32>,budget:usize}
impl<'a> Decoder<'a>{
    fn take(&mut self,n:usize)->Option<&'a[u8]>{let end=self.pos.checked_add(n)?;let b=self.data.get(self.pos..end)?;self.pos=end;Some(b)}
    fn number(&mut self,n:usize)->Option<u64>{Some(self.take(n)?.iter().fold(0,|v,b|(v<<8)|*b as u64))}
    fn string(&mut self,n:usize)->Option<Value>{Some(Value::String(std::str::from_utf8(self.take(n)?).ok()?.into()))}
    fn record(&mut self,id:i32,depth:usize)->Option<Value>{
        let keys=self.records.get(&id)?.clone();let mut map=Map::new();
        for key in keys {map.insert(key,self.value(depth+1)?);}Some(Value::Object(map))
    }
    fn value(&mut self,depth:usize)->Option<Value>{
        if depth>64 || self.budget==0{return None;}self.budget-=1;
        let t=self.number(1)? as u8;
        match t {
            0x40..=0x7f if self.records.contains_key(&((t-0x40) as i32))=>{
                let mut id=(t-0x40) as i32;
                if self.two.contains(&id){let high=self.number(1)? as i32;id=if id<32{-((high<<5)+id)}else{(high<<5)+id};}
                self.record(id,depth)
            },
            0x00..=0x7f=>Some(Value::from(t)),0xe0..=0xff=>Some(Value::from(t as i8)),
            0xc0=>Some(Value::Null),0xc2=>Some(Value::Bool(false)),0xc3=>Some(Value::Bool(true)),
            0xa0..=0xbf=>self.string((t&31) as usize),
            0xd9..=0xdb=>{let n=self.number(1<<(t-0xd9))? as usize;self.string(n)},
            0xcc..=0xcf=>Some(Value::from(self.number(1<<(t-0xcc))?)),
            0xd0..=0xd3=>{let n=1usize<<(t-0xd0);let v=self.number(n)?;Some(Value::from(((v<<(64-n*8)) as i64)>>(64-n*8)))},
            0xca=>{let v=f32::from_bits(self.number(4)? as u32);serde_json::Number::from_f64(v as f64).map(Value::Number)},
            0xcb=>{let v=f64::from_bits(self.number(8)?);serde_json::Number::from_f64(v).map(Value::Number)},
            0x90..=0x9f|0xdc|0xdd=>{
                let n=if t<0xa0{(t&15) as usize}else{self.number(if t==0xdc{2}else{4})? as usize};
                if n>self.budget{return None;}let mut a=Vec::new();for _ in 0..n{a.push(self.value(depth+1)?);}Some(Value::Array(a))
            },
            0x80..=0x8f|0xde|0xdf=>{
                let n=if t<0x90{(t&15) as usize}else{self.number(if t==0xde{2}else{4})? as usize};
                if n>self.budget/2{return None;}let mut map=Map::new();for _ in 0..n{let key=self.value(depth+1)?.as_str()?.to_string();map.insert(key,self.value(depth+1)?);}Some(Value::Object(map))
            },
            0xd4|0xd5=>{
                let kind=self.number(1)?;let first=self.number(1)? as i32;
                if kind==0 && t==0xd4{return Some(Value::Null);}
                if kind!=0x72{return None;}
                let first=first&63;let id=if t==0xd5{let high=self.number(1)? as i32;self.two.insert(first);if first<32{-((high<<5)+first)}else{(high<<5)+first}}else{first};
                let keys=self.value(depth+1)?.as_array()?.iter().map(|v|v.as_str().map(str::to_string)).collect::<Option<Vec<_>>>()?;
                self.records.insert(id,keys);self.record(id,depth)
            },
            _=>None,
        }
    }
}
fn decode(data:&[u8])->Option<Value>{
    let mut d=Decoder{data,pos:0,records:HashMap::new(),two:HashSet::new(),budget:200000};
    let v=d.value(0)?;(d.pos==data.len()).then_some(v)
}

fn normalized(s:&str)->String{s.chars().flat_map(char::to_lowercase).filter(|c|c.is_alphanumeric()).collect()}
fn select(value:&Value,title:&str,artist:&str,duration:f64)->Option<String>{
    let detail=value.pointer("/info/mediaDetail")?;let track=detail.get("playable")?;
    if normalized(track.get("name")?.as_str()?)!=normalized(title){return None;}
    let artists=track.get("artists")?.as_array()?.iter().map(|v|v.get("name")?.as_str()).collect::<Option<Vec<_>>>()?.join(",");
    if super::artist_set(&artists).is_empty() || super::artist_set(&artists)!=super::artist_set(artist){return None;}
    let actual=track.get("duration")?.as_f64()?/1000.0;
    if duration>0.0 && (actual-duration).abs()>2.0{return None;}
    let lyric=detail.get("lyrics")?;
    if !matches!(lyric.get("type")?.as_str()?,"krc"|"lrc"){return None;}
    lyric.get("content")?.as_str().filter(|s|!s.is_empty()).map(str::to_string)
}
pub fn read(title:&str,artist:&str,duration:f64)->Option<String>{
    let path=std::path::PathBuf::from(std::env::var_os("APPDATA")?).join("SodaMusic/LunaCacheV2/entries.db");
    // 复制快照后校验元页，客户端写入期间放弃本次读取，由后续轮询重试。
    use std::io::Read;
    let mut file=std::fs::File::open(&path).ok()?;
    if file.metadata().ok()?.len()>32*1024*1024{return None;}
    let mut data=Vec::new();file.read_to_end(&mut data).ok()?;
    let size=le(&data,48,4)?;if !(512..=65536).contains(&size){return None;}
    let mut headers=vec![0;size*2];std::fs::File::open(path).ok()?.read_exact(&mut headers).ok()?;
    if data.get(..headers.len())?!=headers{return None;}
    for entry in entries(&data)? {if let Some(value)=decode(entry){if let Some(raw)=select(&value,title,artist,duration){return Some(raw);}}}
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn duet_order_is_not_a_different_recording(){
        let v=serde_json::json!({"info":{"mediaDetail":{"playable":{"name":"合唱","artists":[{"name":"甲"},{"name":"乙"}],"duration":120000},"lyrics":{"type":"krc","content":"[1000,500]<0,500,0>词"}}}});
        assert!(select(&v,"合唱","乙/甲",120.0).is_some());
        for artist in ["甲","甲/丙",""] {assert!(select(&v,"合唱",artist,120.0).is_none());}
    }
    #[test] fn record_and_invalid_input(){
        let data=b"\xd4r\x40\x91\xa4name\xa4song";
        assert_eq!(decode(data),Some(serde_json::json!({"name":"song"})));
        for end in 0..data.len(){assert!(decode(&data[..end]).is_none());}
        assert!(entries(&[0;4096]).is_none());
    }
    #[test] fn snapshot_follows_branch_and_overflow_and_rejects_corruption(){
        let mut db=vec![0u8;4096*6];
        fn put(db:&mut[u8],p:usize,v:usize,n:usize){for i in 0..n{db[p+i]=(v>>(8*i)) as u8;}}
        for meta in [0,4096]{put(&mut db,meta+24,0xbeefc0de,4);put(&mut db,meta+28,2,4);put(&mut db,meta+48,4096,4);}
        put(&mut db,136,2,8);put(&mut db,152,2,8);
        put(&mut db,4096+136,5,8);put(&mut db,4096+152,1,8);
        for (page,kind) in [(2,1),(3,2),(4,4)]{put(&mut db,page*4096,page,8);put(&mut db,page*4096+18,kind,2);}
        for page in [2,3]{put(&mut db,page*4096+20,2,2);put(&mut db,page*4096+24,100,2);}
        put(&mut db,2*4096+124,3,4);
        let node=3*4096+124;let payload=b"\xd4r\x40\x91\xa4name\xa4song";
        put(&mut db,node,payload.len(),4);put(&mut db,node+4,1,2);put(&mut db,node+8,4,8);
        put(&mut db,4*4096+20,1,4);db[4*4096+24..4*4096+24+payload.len()].copy_from_slice(payload);
        assert_eq!(entries(&db).unwrap(),vec![payload.as_slice()]);
        assert!(entries(&db[..4*4096]).is_none());
        put(&mut db,node,8192,4);assert!(entries(&db).is_none());
        put(&mut db,node,payload.len(),4);put(&mut db,2*4096+124,2,4);assert!(entries(&db).is_none());
    }
    #[test] fn rejects_other_versions(){
        let v=serde_json::json!({"info":{"mediaDetail":{"playable":{"name":"歌 (Live)","artists":[{"name":"歌手"}],"duration":120000},"lyrics":{"type":"krc","content":"[1000,500]<0,500,0>词"}}}});
        assert!(select(&v,"歌 (Live)","歌手",120.0).is_some());
        assert!(select(&v,"歌","歌手",120.0).is_none());
        assert!(select(&v,"歌 (Live)","他人",120.0).is_none());
        assert!(select(&v,"歌 (Live)","歌手",150.0).is_none());
    }
}
