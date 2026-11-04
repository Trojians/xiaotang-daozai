//! 按显示屏的物理坐标居中岛体，透明画布的额外宽度不参与居中。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds { pub left:i32, pub top:i32, pub right:i32, pub bottom:i32 }

#[derive(Clone, Debug)]
pub struct Display { pub id:String, pub label:String, pub bounds:Bounds, pub primary:bool }

pub fn selected<'a>(displays:&'a [Display],id:&str)->Option<&'a Display>{
    displays.iter().find(|d|!id.is_empty()&&d.id==id)
        .or_else(||displays.iter().find(|d|d.primary)).or_else(||displays.first())
}

#[cfg(windows)]
pub fn connected()->Vec<Display>{
    use windows::{core::PCWSTR,Win32::{Foundation::{BOOL,LPARAM,RECT},Graphics::Gdi::*,UI::WindowsAndMessaging::EDD_GET_DEVICE_INTERFACE_NAME}};
    fn text(value:&[u16])->String {String::from_utf16_lossy(&value[..value.iter().position(|c|*c==0).unwrap_or(value.len())])}
    unsafe extern "system" fn collect(monitor:HMONITOR,_:HDC,_:*mut RECT,data:LPARAM)->BOOL {
        let mut info=MONITORINFOEXW::default();info.monitorInfo.cbSize=std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(monitor,&mut info.monitorInfo).as_bool(){
            let device=text(&info.szDevice);
            let mut details=DISPLAY_DEVICEW{cb:std::mem::size_of::<DISPLAY_DEVICEW>() as u32,..Default::default()};
            let found=EnumDisplayDevicesW(PCWSTR(info.szDevice.as_ptr()),0,&mut details,EDD_GET_DEVICE_INTERFACE_NAME).as_bool();
            let stable_id=if found{text(&details.DeviceID)}else{String::new()};
            let name=if found{text(&details.DeviceString)}else{String::new()};
            let number=device.trim_start_matches(r"\\.\DISPLAY");let r=info.monitorInfo.rcMonitor;
            let primary=info.monitorInfo.dwFlags&1!=0;
            let label=format!("显示器 {number}{} · {} × {}{}",if name.is_empty(){String::new()}else{format!(" · {name}")},r.right-r.left,r.bottom-r.top,if primary{" · 主屏"}else{""});
            let displays=&mut *(data.0 as *mut Vec<Display>);
            displays.push(Display{id:if stable_id.is_empty(){device}else{stable_id},label,bounds:Bounds{left:r.left,top:r.top,right:r.right,bottom:r.bottom},primary});
        }
        BOOL(1)
    }
    let mut displays:Vec<Display>=Vec::new();
    unsafe {let _=EnumDisplayMonitors(None,None,Some(collect),LPARAM(&mut displays as *mut Vec<Display> as isize));}
    displays.sort_by(|a,b|a.label.cmp(&b.label));displays
}

#[cfg(not(windows))]
pub fn connected()->Vec<Display>{Vec::new()}

pub fn island_origin(m:Bounds, island_width:f32, scale:f32)->Option<(i32,i32)> {
    if m.right<=m.left || m.bottom<=m.top || !scale.is_finite() || scale<=0.0 || !island_width.is_finite() || island_width<=0.0 {return None;}
    let width=(island_width as f64*scale as f64).round();
    let x=(m.left as f64+(m.right as f64-m.left as f64-width)/2.0).round();
    Some((x.clamp(i32::MIN as f64,i32::MAX as f64) as i32,m.top))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test] fn connected_displays_have_valid_bounds_and_distinct_ids(){
        let displays=connected();
        for (i,display) in displays.iter().enumerate(){
            println!("{}：{:?}",display.label,display.bounds);
            assert!(!display.id.is_empty());assert!(island_origin(display.bounds,640.0,1.0).is_some());
            assert!(!displays[..i].iter().any(|d|d.id==display.id));
        }
        if !displays.is_empty(){assert_eq!(displays.iter().filter(|d|d.primary).count(),1);}
    }
    #[test] fn display_selection_survives_reordering_disconnect_and_reconnect(){
        let primary=Display{id:"internal".into(),label:"内屏".into(),bounds:Bounds{left:0,top:0,right:1920,bottom:1080},primary:true};
        let external=Display{id:"external".into(),label:"外屏".into(),bounds:Bounds{left:-2560,top:-400,right:0,bottom:1040},primary:false};
        for displays in [vec![primary.clone(),external.clone()],vec![external.clone(),primary.clone()]] {
            assert_eq!(selected(&displays,"").unwrap().id,"internal");
            let target=selected(&displays,"external").unwrap();assert_eq!(target.id,"external");
            assert_eq!(island_origin(target.bounds,640.0,1.5),Some((-1760,-400)));
        }
        assert_eq!(selected(&[primary.clone()],"external").unwrap().id,"internal");
        assert_eq!(selected(&[primary,external],"external").unwrap().id,"external");
        assert!(selected(&[],"external").is_none());
    }
    #[test] fn real_origins_and_dpi_center_only_the_island(){
        for (m,scale,want) in [
            (Bounds{left:0,top:0,right:1920,bottom:1080},1.0,(640,0)),
            (Bounds{left:-2560,top:-400,right:0,bottom:1040},1.5,(-1760,-400)),
            (Bounds{left:300,top:-1440,right:2860,bottom:0},2.0,(940,-1440)),
            (Bounds{left:1920,top:200,right:3840,bottom:1280},1.25,(2480,200)),
        ] {assert_eq!(island_origin(m,640.0,scale),Some(want));}
    }
    #[test] fn changing_primary_unplugging_and_dpi_recompute_without_stale_state(){
        let a=Bounds{left:0,top:0,right:1920,bottom:1080};
        let b=Bounds{left:-2560,top:0,right:0,bottom:1440};
        assert_ne!(island_origin(a,640.0,1.0),island_origin(b,640.0,1.5));
        assert_eq!(island_origin(a,640.0,1.0),Some((640,0)));
        assert_eq!(island_origin(a,640.0,2.0),Some((320,0)));
        assert_eq!(island_origin(a,640.0,f32::NAN),None);
    }
}

/// 只有岛仔所在屏幕的前台全屏窗口才触发隐藏。
pub fn hide_for_fullscreen(enabled:bool,fullscreen:Option<Bounds>,island:Option<Bounds>)->bool {
    enabled && fullscreen.is_some() && fullscreen==island
}

#[cfg(test)]
mod fullscreen_tests {
    use super::*;
    #[test]
    fn fullscreen_on_another_monitor_keeps_island_visible(){
        let primary=Bounds{left:0,top:0,right:1920,bottom:1080};
        for secondary in [Bounds{left:-2560,top:0,right:0,bottom:1440},Bounds{left:1920,top:-200,right:4480,bottom:1240},Bounds{left:0,top:-1440,right:2560,bottom:0}] {
            assert!(!hide_for_fullscreen(true,Some(secondary),Some(primary)));
            assert!(!hide_for_fullscreen(true,Some(primary),Some(secondary)));
            assert!(hide_for_fullscreen(true,Some(secondary),Some(secondary)));
        }
        assert!(hide_for_fullscreen(true,Some(primary),Some(primary)));
        assert!(!hide_for_fullscreen(false,Some(primary),Some(primary)));
        assert!(!hide_for_fullscreen(true,None,Some(primary)));
        assert!(!hide_for_fullscreen(true,None,None));
        assert!(!hide_for_fullscreen(true,Some(primary),None));
    }
}
