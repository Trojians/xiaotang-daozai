//! 五个页面：音乐 → 专注 → 饮水 → 置物 → 待办。
//! 坐标全部是面板局部逻辑点（419×140），文字 y 是基线。

use crate::anim::{self, lerp, smoothstep, Spring};
use crate::app::{App, Cmd, Env, Phase};
use crate::mail;
use crate::paint::{self, capsule, mix, Card, H};
use crate::smtc::Transport;
use crate::theme::{self, size, W};
use eframe::egui::{self, Color32, Pos2, Rect};
use std::f32::consts::{FRAC_PI_2, TAU};

fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// 由标题生成稳定的封面渐变色。
fn cover_colors(title: &str) -> (Color32, Color32) {
    let mut h: u64 = 1469598103934665603;
    for b in title.bytes() {
        h = (h ^ b as u64).wrapping_mul(1099511628211);
    }
    let palette = [
        (Color32::from_rgb(255, 94, 98), Color32::from_rgb(255, 153, 102)),
        (Color32::from_rgb(86, 204, 242), Color32::from_rgb(47, 128, 237)),
        (Color32::from_rgb(168, 85, 247), Color32::from_rgb(236, 72, 153)),
        (Color32::from_rgb(52, 211, 153), Color32::from_rgb(16, 120, 200)),
        (Color32::from_rgb(251, 191, 36), Color32::from_rgb(239, 68, 68)),
    ];
    palette[(h % palette.len() as u64) as usize]
}

impl App {
    // ================= 音乐 =================
    pub(crate) fn page_music(&mut self, c: &Card, _env: &Env, cmds: &mut Vec<Cmd>) {
        let np=self.media.clone();
        let has=!np.title.is_empty();
        let with_lyrics=has && self.show_lyrics;
        let ctx=c.ui.ctx().clone();
        let playing_t=ctx.animate_bool_with_time(egui::Id::new("np-playing"),np.playing,0.22);
        let art_size=if with_lyrics {44.0}else{62.0};
        let art_y=if with_lyrics {23.0}else{13.0};
        if let Some((image,texture))=&self.music_artwork {
            let [w,h]=image.size;
            let (uw,vh)=if w>h {(h as f32/w as f32,1.0)}else{(1.0,w as f32/h as f32)};
            let uv=Rect::from_center_size(Pos2::new(0.5,0.5),egui::vec2(uw,vh));
            c.painter.add(egui::epaint::RectShape::filled(c.rect(44.0,art_y,art_size,art_size),10.0*c.s,Color32::WHITE).with_texture(texture.id(),uv));
        } else if has {
            let (a,b)=cover_colors(&np.title);
            let cc=c.clipped(44.0,art_y,art_size,art_size);
            cc.vgrad(44.0,art_y,art_size,art_size,10.0,a,b);
            paint::note(&cc,44.0+art_size/2.0,art_y+art_size/2.0,art_size*0.39,Color32::from_white_alpha(235));
        } else {
            c.fill(44.0,13.0,62.0,62.0,13.0,Color32::from_rgb(25,25,27));
            paint::note(c,75.0,44.0,26.0,Color32::from_rgb(110,110,114));
        }

        if !has {
            c.text(121.0,47.0,"暂无音乐",14.0,W::Bold,theme::WHITE,H::Left);
        } else {
            let text_x=if with_lyrics {99.0}else{121.0};
            let text_width=if with_lyrics {123.0}else{220.0};
            let title_size=if with_lyrics {12.0}else{14.0};
            c.text(text_x,if with_lyrics {41.0}else{53.0},&c.fit(&np.title,title_size,W::Bold,text_width),title_size,W::Bold,theme::WHITE,H::Left);
            c.text(text_x,if with_lyrics {57.0}else{67.0},&c.fit(&np.artist,size::BODY,W::Regular,text_width),size::BODY,W::Regular,theme::GRAY,H::Left);
            if with_lyrics {self.draw_music_lyrics(c);}
        }

        let tc=if has {theme::WHITE}else{Color32::from_rgb(78,78,82)};
        let (bx,bw,by)=if with_lyrics {(44.0,176.0,82.0)}else{(96.0,221.0,92.6)};
        let bar=c.hit_drag(bx-4.0,by-7.0,bw+8.0,14.0,"m-seek");
        let seek_position=if has && np.duration>0.0 && (bar.is_pointer_button_down_on() || bar.clicked() || bar.drag_stopped()) {
            bar.interact_pointer_pos().or_else(||ctx.input(|i|i.pointer.interact_pos()))
                .map(|pointer|((c.to_local(pointer).x-bx)/bw).clamp(0.0,1.0) as f64*np.duration)
        }else{None};
        let position=seek_position.unwrap_or(np.position);
        let frac=if np.duration>0.0 {(position/np.duration).clamp(0.0,1.0) as f32}else{0.0};
        let (time_y,time_size,right)=if with_lyrics {(100.0,10.0,220.0)}else{(97.0,15.0,375.0)};
        c.text(44.0,time_y,&mmss(position),time_size,W::Regular,tc,H::Left);
        let remaining=if has && np.duration<=0.0{"--:--".into()}else{format!("-{}",mmss((np.duration-position).max(0.0)))};
        c.text(right,time_y,&remaining,time_size,W::Regular,tc,H::Right);
        let (bh,_)=c.hover_t(&bar);
        let bh=if has {bh}else{0.0};
        let th=lerp(3.5,5.0,bh);
        c.fill(bx,by-th/2.0,bw,th,th/2.0,if has {Color32::from_gray(90)}else{theme::TRACK});
        if frac>0.0 {c.fill(bx,by-th/2.0,(bw*frac).max(th),th,th/2.0,theme::WHITE);}
        if bh>0.01 {c.faded(bh).circle(bx+bw*frac,by,4.5,theme::WHITE);}
        if bar.clicked() || bar.drag_stopped() {
            if let Some(position)=seek_position {cmds.push(Cmd::Media(Transport::Seek(position)));}
        }

        let y=118.0;
        let ic=if has {theme::WHITE}else{Color32::from_rgb(88,88,92)};
        let btn=|x:f32,w:f32,id:&str| {
            let response=c.hit(x-w/2.0,y-12.0,w,24.0,id);
            let (hover,pressed)=c.hover_t(&response);
            if has && hover>0.01 {c.faded(hover).circle(x,y,12.5,Color32::from_white_alpha(20));}
            (response,c.scaled(x,y,1.0-0.1*pressed))
        };
        let (lyrics_button,lyrics_card)=btn(74.0,26.0,"m-lyr");
        lyrics_card.text(74.0,y+5.0,"词",14.0,W::Semibold,if !has {ic}else if self.show_lyrics {theme::WHITE}else{theme::GRAY},H::Center);
        let (previous,previous_card)=btn(162.0,30.0,"m-prev");
        paint::skip(&previous_card,162.0,y,-1.0,ic);
        let (play,play_card)=btn(210.5,30.0,"m-play");
        paint::play_pause(&play_card,211.0,y,15.0,playing_t,ic);
        let (next,next_card)=btn(258.0,30.0,"m-next");
        paint::skip(&next_card,258.0,y,1.0,ic);
        let (volume_button,volume_card)=btn(367.0,26.0,"m-volume");
        let (volume,muted)=self.volume_ui.unwrap_or((0,false));
        paint::speaker(&volume_card,367.0,y,muted,ic);
        if self.volume_ui.is_some() && volume_button.on_hover_text(if muted {"取消静音"}else{"静音"}).clicked() {
            self.volume_ui=Some((volume,!muted));
            self.volume_quiet_until=self.t+0.9;
            cmds.push(Cmd::ToggleMute);
        }
        if has {
            if previous.clicked() {cmds.push(Cmd::Media(Transport::Prev));}
            if play.clicked() {cmds.push(Cmd::Media(Transport::Toggle));}
            if next.clicked() {cmds.push(Cmd::Media(Transport::Next));}
        }
        if lyrics_button.on_hover_text(if self.show_lyrics {"隐藏歌词"}else{"显示歌词"}).clicked() {
            if !has {self.say("先放一首歌吧");}
            else {
                self.show_lyrics=!self.show_lyrics;
                self.settings.lyrics_enabled=self.show_lyrics;
                self.settings.save();
            }
        }
        if has {
            let (button,button_card)=capsule(c,279.0,108.0,68.0,20.0,"m-island-lyrics",if self.settings.island_lyrics {Color32::from_gray(39)}else{Color32::from_gray(22)},Color32::from_gray(47));
            button_card.text(313.0,122.0,"岛上歌词",10.0,W::Semibold,if self.settings.island_lyrics {theme::WHITE}else{theme::GRAY},H::Center);
            if button.on_hover_text(if self.settings.island_lyrics {"关闭收起时的歌词"}else{"收起时显示当前歌词"}).clicked() {
                self.settings.island_lyrics=!self.settings.island_lyrics;
                self.settings.save();
            }
        }
    }

    fn draw_music_lyrics(&mut self,c:&Card) {
        let col=c.clipped(243.0,28.0,136.0,74.0);
        c.seg((233.0,32.0),(233.0,98.0),1.0,Color32::from_gray(35));
        if let Some(lyrics)=&self.ly.lyrics {
            let current=if lyrics.synced {lyrics.index_at(self.lyric_position())}else{Some(0)};
            let target=current.unwrap_or(0);
            let position=c.ui.ctx().animate_value_with_time(egui::Id::new(("music-lyrics-scroll",&self.ly.key)),target as f32,0.3);
            let first=(position.floor() as usize).saturating_sub(2);
            for (index,line) in lyrics.lines.iter().enumerate().skip(first).take(6) {
                let distance=index as f32-position;
                if distance.abs()>2.2 {continue;}
                let active=current==Some(index);
                let ink=if active {theme::WHITE}else{theme::GRAY};
                let row=col.faded((1.0-distance.abs()*0.35).clamp(0.0,1.0));
                let mut job=egui::text::LayoutJob::simple(line.text.clone(),row.font(if active {12.0}else{10.0},if active {W::Semibold}else{W::Regular}),ink,132.0*c.s);
                job.wrap.max_rows=if active {3}else{1};
                job.wrap.break_anywhere=true;
                let galley=row.painter.layout_job(job);
                let y=64.0+distance*29.0-galley.size().y/(2.0*c.s);
                row.painter.galley(row.pos(246.0,y),galley,ink);
            }
            if !lyrics.synced {col.text(246.0,39.0,"文本歌词",8.0,W::Regular,theme::GRAY,H::Left);}
        } else {
            let status=if self.ly.loading {"正在查找歌词…"}else if let Some(error)=&self.ly.error {error.label()}else{"暂无歌词"};
            let galley=col.painter.layout(status.into(),col.font(10.0,W::Regular),theme::GRAY,130.0*c.s);
            col.painter.galley(col.pos(246.0,61.0-galley.size().y/(2.0*c.s)),galley,theme::GRAY);
            if self.ly.error.is_some() {
                col.text(246.0,87.0,"点击重试",9.0,W::Regular,theme::GRAY,H::Left);
                if c.hit(243.0,28.0,136.0,74.0,"lyrics-retry").on_hover_text("重新查询当前歌曲的歌词").clicked(){self.ly.key.clear();}
            }
        }
    }

    // ================= 专注（番茄钟） =================
    pub(crate) fn page_focus(&mut self, c: &Card, _env: &Env) {
        if c.hit(90.0,8.0,42.0,21.0,"focus-settings").on_hover_text("专注设置").clicked(){self.open_settings_category(2);}
        c.text(111.0,23.0,"设置",10.5,W::Regular,theme::GRAY,H::Center);
        let (rx, ry, rr) = (79.0, 74.0, 32.0);
        let frac = self.ring_shown.pos.clamp(0.0, 1.0);
        let run = c.ui.ctx().animate_bool_with_time(egui::Id::new("pomo-run"), self.pomo.running, 0.3);
        let resting=self.pomo.phase!=Phase::Focus;
        let (top,bottom)=if resting{(theme::SOFT_GREEN,mix(theme::SOFT_GREEN,Color32::BLACK,0.2))}else{(theme::RED_TOP,theme::RED)};
        // 外圈柔光；计时中缓慢呼吸
        let breathe = 0.5 + 0.5 * ((self.t * 2.2).sin() as f32);
        let glow = 0.30 + 0.25 * run * breathe;
        c.glow(rx, ry, 48.0, paint::alpha(bottom, glow * 0.55));
        c.ring(rx, ry, rr, 7.0, if resting{mix(theme::CARD,top,0.18)}else{Color32::from_rgb(58,22,26)});
        // 渐变进度环随当前阶段配色。
        if frac > 0.001 {
            let steps = (120.0 * frac).ceil().max(2.0) as usize;
            let a0 = -FRAC_PI_2;
            let mut prev = (rx + rr * a0.cos(), ry + rr * a0.sin());
            for i in 1..=steps {
                let a = a0 + TAU * frac * i as f32 / steps as f32;
                let next = (rx + rr * a.cos(), ry + rr * a.sin());
                let t = ((prev.1 + next.1) * 0.5 - (ry - rr)) / (2.0 * rr);
                c.seg(prev, next, 7.0, mix(top, bottom, t));
                prev = next;
            }
            // 圆头
            c.circle(rx, ry - rr, 3.5, top);
            c.circle(prev.0, prev.1, 3.5, mix(top, bottom, (prev.1 - (ry - rr)) / (2.0 * rr)));
        }
        let hand = if self.pomo.running { (self.t as f32 * 1.2) % TAU } else { 0.0 };
        if resting {
            let t=if self.pomo.running && crate::bar_layout::animations_enabled(){self.t as f32}else{0.0};
            crate::focus_bar::tea(&c.scaled(rx,ry,2.2),rx,ry,frac,t);
        }else{paint::stopwatch(c, rx, ry + 0.5, 15.5, hand * run, theme::WHITE);}

        let secs = self.pomo.remaining.max(0.0).ceil() as u32;
        c.text(264.0, 61.0, &format!("{:02}:{:02}", secs / 60, secs % 60), size::HERO + 1.0, W::Bold, if resting{theme::SOFT_GREEN}else{theme::WHITE}, H::Center);
        c.text(264.0, 79.0, self.pomo.phase.label(), size::SMALL, W::Regular, theme::GRAY, H::Center);

        // 重置：点一下图标转一圈
        let r = c.hit(317.0, 38.0, 24.0, 24.0, "f-reset");
        let (h, p) = c.hover_t(&r);
        let rc = c.scaled(329.0, 50.0, 1.0 - 0.1 * p);
        rc.circle(329.0, 50.0, 11.0, mix(Color32::from_rgb(28, 28, 30), Color32::from_rgb(50, 50, 54), h));
        paint::reset(&rc, 329.0, 50.0, -self.reset_spin.pos, Color32::from_gray(230));
        if r.clicked() {
            self.pomo.remaining = self.phase_secs(self.pomo.phase);
            self.pomo.running = false;
            self.reset_spin.target += TAU;
        }

        for (phase,label,x,width,id) in [(Phase::Focus,"专注",148.0,88.0,"f-focus"),(Phase::Rest,"休息",292.0,88.0,"f-rest")] {
            let selected=self.pomo.phase==phase;
            let accent=if phase==Phase::Focus{theme::RED_TOP}else{theme::SOFT_GREEN};
            let background=if selected{mix(theme::BTN,accent,0.22)}else{theme::BTN};
            let (response,button)=capsule(c,x,94.0,width,25.0,id,background,if selected{mix(theme::BTN_HOVER,accent,0.25)}else{theme::BTN_HOVER});
            button.text(x+width/2.0,110.0,label,size::BODY,W::Semibold,if selected{mix(accent,theme::WHITE,0.5)}else{theme::WHITE},H::Center);
            if response.clicked(){self.set_phase(phase);}
        }
        let (b2, c2) = capsule(c, 246.0, 94.0, 36.0, 25.0, "f-start", theme::RED, Color32::from_rgb(255, 82, 98));
        paint::play_pause(&c2, 264.0, 106.5, 11.0, run, theme::WHITE);
        let b2=b2.on_hover_text(if self.pomo.running{"暂停计时"}else{"开始计时"});
        if b2.clicked() {
            if self.pomo.remaining <= 0.0 {
                self.pomo.remaining = self.phase_secs(self.pomo.phase);
            }
            self.pomo.running = !self.pomo.running;
        }
    }

    fn set_phase(&mut self, ph: Phase) {
        if self.pomo.phase==ph {return;}
        self.pomo.phase = ph;
        self.pomo.remaining = self.phase_secs(ph);
        self.pomo.running = false;
    }

    // ================= 饮水 =================
    fn next_hint(&self, env: &Env) -> String {
        let left = (self.settings.water_minutes as f64 * 60.0 - (self.t - self.last_reminder)).max(0.0) as u32 / 60;
        let total = env.hour() as u32 * 60 + env.minute() + left;
        format!("{:02}:{:02}", (total / 60) % 24, total % 60)
    }

    pub(crate) fn page_water(&mut self, c: &Card, env: &Env) {
        let pct = self.water.percent();
        let liters = self.water_ml / 1000.0;
        let ww = c.text(44.0, 61.0, &format!("{liters:.1}"), size::HERO, W::Bold, theme::WHITE, H::Left);
        c.text(44.0 + ww + 5.0, 60.0, &format!("/ {:.1} L", self.water.goal_ml as f32 / 1000.0), 11.0, W::Semibold, theme::GRAY, H::Left);

        // 提醒开关：旋钮弹簧滑动
        let sw = c.hit(160.0, 35.0, 38.0, 22.0, "w-remind");
        let (sh, _) = c.hover_t(&sw);
        let on = self.remind_t.pos;
        let track = mix(Color32::from_rgb(58, 58, 62), theme::BLUE, on.clamp(0.0, 1.0));
        c.fill(162.0, 36.0, 34.0, 20.0, 10.0, mix(track, theme::WHITE, sh * 0.08));
        let kx = lerp(172.0, 186.0, on);
        c.circle(kx, 46.0, 8.0, theme::WHITE);
        paint::bell(&c.scaled(kx, 46.0, 0.9), kx, 46.0, mix(Color32::from_gray(140), theme::BLUE, on.clamp(0.0, 1.0)),0.0);
        if sw.clicked() {
            self.water.remind = !self.water.remind;
            self.water.save();
            self.last_reminder = self.t;
            self.say(if self.water.remind { "饮水提醒已打开" } else { "饮水提醒已关闭" });
        }

        // 今天 · 3% · 下次 08:07
        let mut x = 44.0;
        x += c.text(x, 77.0, "今天 · ", size::SMALL, W::Regular, theme::GRAY, H::Left);
        x += c.text(x, 77.0, &format!("{pct:.0}%"), size::SMALL, W::Bold, theme::BLUE, H::Left);
        if self.water.percent()>=100.0 {
            c.text(x,77.0," · 已达标",size::SMALL,W::Regular,theme::GRAY,H::Left);
        } else if self.water.remind && self.water_hours(env.hour() as u32) {
            c.text(x, 77.0, &format!(" · 下次 {}", self.next_hint(env)), size::SMALL, W::Regular, theme::GRAY, H::Left);
        }
        c.fill(44.0, 84.0, 151.0, 3.5, 1.75, Color32::from_rgb(34, 34, 37));
        let f = self.water_ring.pos.clamp(0.0, 1.0);
        if f > 0.0 {
            c.fill(44.0, 84.0, (151.0 * f).max(3.5), 3.5, 1.75, theme::BLUE);
        }

        let (d, dc) = capsule(c, 43.0, 98.0, 62.0, 25.0, "w-drink", theme::BLUE_DEEP, Color32::from_rgb(22, 92, 165));
        paint::droplet(&dc, 56.0, 110.5, 11.0, Color32::from_rgb(120, 200, 255));
        dc.text(65.0, 114.0, "喝一口", size::BODY, W::Semibold, Color32::from_rgb(220, 236, 255), H::Left);
        let (s, sc) = capsule(c, 111.0, 98.0, 66.0, 25.0, "w-set", theme::BTN, theme::BTN_HOVER);
        paint::sliders(&sc, 126.0, 110.5, theme::WHITE);
        sc.text(149.0, 114.0, "设置", size::BODY, W::Regular, theme::WHITE, H::Center);
        if d.clicked() { self.drink_water(); }
        if s.clicked() { self.open_settings_category(1); }
        if self.last_drink.is_some() {
            c.text(191.0,114.0,"撤销",10.5,W::Regular,theme::GRAY,H::Center);
            if c.hit(177.0,98.0,28.0,25.0,"water-undo").on_hover_text("撤销最近一次饮水记录").clicked() {
                if let Some((ml,hour))=self.last_drink.take() {
                    self.water.drunk_ml=self.water.drunk_ml.saturating_sub(ml);
                    self.water.hours[hour]=self.water.hours[hour].saturating_sub(ml);
                    self.water.save(); self.say("已撤销这次记录");
                }
            }
        }

        c.fill(208.0, 36.0, 1.0, 86.0, 0.5, Color32::from_rgb(30, 30, 33));

        // 右侧：环形 ↔ 柱状图（点右侧区域或右上角两颗豆切换）
        let vt = self.water_view_t.pos;
        let area = c.hit(214.0, 30.0, 166.0, 96.0, "w-view");
        if area.clicked() {
            self.water_view = 1 - self.water_view;
        }
        for i in 0..2 {
            let on = if i == 0 { 1.0 - vt.clamp(0.0, 1.0) } else { vt.clamp(0.0, 1.0) };
            let w = lerp(3.0, 7.0, on);
            let x = 362.0 + i as f32 * 8.0 - w / 2.0 + if i == 0 { -1.0 * on } else { 1.0 * on };
            c.fill(x, 33.5, w, 3.0, 1.5, mix(Color32::from_gray(80), theme::WHITE, on));
        }

        let ring_a = 1.0 - smoothstep(0.0, 0.6, vt);
        if ring_a > 0.0 {
            let k = 1.0 + 0.22 * vt;
            let rc = c.faded(ring_a).scaled(292.0, 78.0, k);
            Self::water_ring(&rc, 292.0, 78.0, 33.0, 6.0, f);
            rc.text(292.0, 86.0, &format!("{pct:.0}"), 26.0, W::Bold, theme::WHITE, H::Center);
            paint::droplet(&rc, 287.0, 97.0, 7.0, theme::CYAN);
            rc.text(292.5, 100.0, "%", 10.5, W::Semibold, Color32::from_gray(217), H::Left);
        }
        let chart_a = smoothstep(0.25, 1.0, vt);
        if chart_a > 0.0 {
            self.water_chart(&c.faded(chart_a), env, vt);
        }
    }

    /// 底部留缺口的进度环（约 276°），起点有圆头亮点。
    fn water_ring(c: &Card, cx: f32, cy: f32, r: f32, thick: f32, frac: f32) {
        let start = 132.0f32.to_radians();
        let sweep = 276.0f32.to_radians();
        c.arc(cx, cy, r, start, start + sweep, thick, theme::WATER_TRACK);
        c.circle(cx + r * start.cos(), cy + r * start.sin(), thick * 0.5, theme::WATER_TRACK);
        c.circle(cx + r * (start + sweep).cos(), cy + r * (start + sweep).sin(), thick * 0.5, theme::WATER_TRACK);
        let end = start + sweep * frac;
        if frac > 0.002 {
            c.arc(cx, cy, r, start, end, thick, theme::BLUE);
        }
        c.circle(cx + r * start.cos(), cy + r * start.sin(), thick * 0.55, theme::BLUE);
        if frac > 0.002 {
            c.circle(cx + r * end.cos(), cy + r * end.sin(), thick * 0.5, theme::BLUE);
        }
    }

    /// 今日分小时柱状图：12 根柱，从当前时间往前；柱子依次升起。
    fn water_chart(&self, c: &Card, env: &Env, vt: f32) {
        let hour = env.hour();
        let first = hour.saturating_sub(8).min(12);
        let (x0, bottom, top) = (224.0, 103.0, 52.0);
        let max = self.water.hours.iter().copied().max().unwrap_or(0).max(500) as f32;
        let scale = if max > 500.0 { (max / 250.0).ceil() * 250.0 } else { 500.0 };
        for (i, label) in [(0.0, format!("{}", scale as u32)), (0.5, format!("{}", (scale / 2.0) as u32)), (1.0, "0 ml".to_string())] {
            let y = lerp(top, bottom, i);
            c.text(343.0, y + 3.0, &label, size::TINY, W::Regular, theme::DIM, H::Left);
            if i == 0.5 {
                for k in 0..28 {
                    c.fill(x0 + k as f32 * 4.0, y, 2.0, 0.8, 0.0, Color32::from_gray(48));
                }
            }
        }
        c.fill(x0 - 2.0, bottom + 1.5, 114.0, 0.8, 0.0, Color32::from_gray(48));
        for i in 0..12 {
            let hr = first + i;
            let x = x0 + i as f32 * 9.4;
            // 依次升起：每根晚 18 ms
            let rise = anim::ease_out_cubic(((vt - 0.3 - i as f32 * 0.03) / 0.5).clamp(0.0, 1.0));
            let full = bottom - top;
            c.fill(x, bottom - full * rise, 5.6, full * rise, 2.8, Color32::from_rgb(26, 26, 29));
            let v = self.water.hours.get(hr).copied().unwrap_or(0) as f32;
            if v > 0.0 {
                let hgt = (full * (v / scale)).max(5.6) * rise;
                let col = if hr == hour { theme::BLUE } else { Color32::from_rgb(40, 120, 190) };
                c.fill(x, bottom - hgt, 5.6, hgt, 2.8, col);
            } else if hr == hour {
                c.circle(x + 2.8, bottom - 2.8, 2.8 * rise, theme::BLUE);
            }
            if hr % 3 == 0 {
                c.text(x + 2.8, 117.0, &format!("{hr:02}"), size::TINY, W::Regular, theme::GRAY, H::Center);
            }
        }
    }

    // ================= 置物架 =================
    pub(crate) fn page_shelf(&mut self, c: &Card, _env: &Env, cmds: &mut Vec<Cmd>) {
        let n = self.shelf.items.len();
        let drag = self.drag_a;
        let title_w = c.text_w("置物架", size::TITLE, W::Bold);
        c.text(344.0,23.0,"添加",size::SMALL,W::Regular,theme::GRAY,H::Right);
        if c.hit(311.0,8.0,38.0,22.0,"shelf-add").on_hover_text("从电脑选择文件").clicked(){cmds.push(Cmd::AddFiles);}
        if n>5 {c.text(209.5,121.0,&format!("{}–{} / {n} · 滚轮查看",self.shelf_first+1,(self.shelf_first+5).min(n)),10.5,W::Regular,theme::DIM,H::Center);}

        if n > 0 {
            // 标题旁的「清空」：第一次点变红，3 秒内再点才真正清空
            let confirming = self.confirm_clear.is_some();
            let label = if confirming { "确认清空" } else { "清空" };
            let lw = c.text_w(label, size::SMALL, W::Regular) + 16.0;
            let base = if confirming { Color32::from_rgb(120, 28, 36) } else { theme::BTN };
            let hover = if confirming { Color32::from_rgb(150, 34, 44) } else { theme::BTN_HOVER };
            let (r, bc) = capsule(&c.faded(1.0 - drag), 15.0 + title_w + 8.0, 11.5, lw, 17.0, "shelf-clear", base, hover);
            bc.text(15.0 + title_w + 8.0 + lw / 2.0, 23.5, label, size::SMALL, W::Regular, theme::WHITE, H::Center);
            if r.clicked() {
                if confirming {
                    self.shelf.clear();
                    self.icons.clear();
                    self.shelf_born.clear();
                    self.shelf_x.clear();
                    self.shelf_first = 0;
                    self.confirm_clear = None;
                    self.say("置物架已清空（文件本身没动）");
                } else {
                    self.confirm_clear = Some(self.t);
                }
            }
        }

        let body = c.faded(lerp(1.0, 0.14, drag));
        if n == 0 {
            let a = 1.0 - drag;
            if a > 0.0 {
                let ec = body.faded(a);
                ec.dashed(42.0, 32.0, 335.0, 91.0, 12.0, 1.2, Color32::from_gray(70), 0.0);
                paint::tray(&ec, 209.5, 60.0, 28.0, 0.0, Color32::from_gray(200), Color32::from_gray(80));
                ec.text(209.5, 93.0, "把文件拖到这儿", 12.0, W::Semibold, theme::WHITE, H::Center);
                ec.text(209.5, 108.0, "只记位置，不复制文件 · 点一下打开", size::TINY, W::Regular, theme::GRAY, H::Center);
            }
        } else {
            self.shelf_row(&body, cmds);
        }
        if drag > 0.002 {
            self.drag_overlay(&c.faded(drag));
        }
    }

    fn shelf_row(&mut self, c: &Card, cmds: &mut Vec<Cmd>) {
        let n = self.shelf.items.len();
        let slot = 68.0;
        let center = 209.5;
        let visible = n.min(5) as f32;
        let scroll = self.shelf_scroll.pos;
        let row = c.clipped(36.0, 28.0, 347.0, 100.0);
        let dt = self.dt;
        let items = self.shelf.items.clone();
        self.icons.retain(&items);
        self.shelf_x.retain(|p, _| items.contains(p));
        for (i, p) in items.iter().enumerate() {
            let target = center + (i as f32 - scroll - (visible - 1.0) / 2.0) * slot;
            let born = self.shelf_born.get(p).copied();
            let s = self.shelf_x.entry(p.clone()).or_insert_with(|| {
                // 新放入的从右侧滑进来；启动时已有的直接就位
                let start = if born.is_some() { target + slot * 0.9 } else { target };
                Spring::new(start, 230.0, 0.9)
            });
            s.target = target;
            s.step(dt);
            let x = s.pos;
            if x < 0.0 || x > 420.0 {
                continue;
            }
            let appear = born.map(|b| smoothstep(0.0, 0.35, (self.t - b) as f32)).unwrap_or(1.0);
            let edge = smoothstep(26.0, 60.0, x) * smoothstep(392.0, 358.0, x);
            let a = appear * edge;
            if a <= 0.01 {
                continue;
            }
            let r = row.hit_drag(x - 31.0, 30.0, 62.0, 78.0, ("shelf", p));
            r.clone().on_hover_text(p.display().to_string());
            let (h, pr) = row.hover_t(&r);
            let cc = row.faded(a).scaled(x, 66.0, 1.0 - 0.04 * pr);
            if h > 0.01 {
                cc.fill(x - 31.0, 30.0, 62.0, 78.0, 10.0, Color32::from_white_alpha((16.0 * h) as u8));
            }
            let lift = -2.0 * h + (1.0 - appear) * 8.0;
            self.file_icon_at(&cc, p, x, 52.0 + lift, 36.0);
            let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let name = cc.fit(&name, size::SMALL, W::Semibold, 60.0);
            cc.text(x, 86.0, &name, size::SMALL, W::Semibold, theme::WHITE, H::Center);
            let meta=self.icons.get(c.ui.ctx(),p).map(|preview|preview.metadata).unwrap_or_default();
            cc.text(x, 99.0, &meta, size::TINY, W::Regular, theme::GRAY, H::Center);
            if r.drag_started() && p.exists() {cmds.push(Cmd::DragFile(p.clone()));}
            let remove=row.hit(x+15.0,30.0,17.0,18.0,("shelf-remove",p));
            let remove_card=row.faded(a);
            remove_card.circle(x+23.5,39.0,7.5,Color32::from_rgb(55,40,46));
            paint::close(&remove_card,x+23.5,39.0,2.4,theme::WHITE);
            let mut delete=remove.on_hover_text("从置物架移除，保留原文件").clicked();
            r.context_menu(|ui|{
                if ui.button("打开").clicked(){if p.exists(){cmds.push(Cmd::Open(p.clone()));}ui.close_menu();}
                if ui.button("在文件夹中显示").clicked(){cmds.push(Cmd::Reveal(p.clone()));ui.close_menu();}
                if ui.button("复制完整路径").clicked(){cmds.push(Cmd::Copy(p.display().to_string()));ui.close_menu();}
                if ui.button("从置物架移除").clicked(){delete=true;ui.close_menu();}
            });
            if delete { self.shelf.remove(p); self.icons.remove(p); self.shelf_x.remove(p); self.shelf_born.remove(p); self.shelf_first=self.shelf_first.min(self.shelf.items.len().saturating_sub(5)); }
            else if r.clicked() { if p.exists(){cmds.push(Cmd::Open(p.clone()));} else {self.say("文件已移动或删除，可从置物架移除");} }
        }
    }

    fn file_icon_at(&mut self, c: &Card, p: &std::path::Path, cx: f32, cy: f32, sz: f32) {
        let tex=self.icons.get(c.ui.ctx(),p).and_then(|preview|preview.image);
        if let Some(tex) = tex {
            let s = tex.size_vec2();
            let k = sz / s.x.max(s.y);
            let r = Rect::from_center_size(c.pos(cx, cy), s * k * c.s);
            c.painter.image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        } else {
            paint::file_glyph(c, paint::file_kind(p), cx, cy, sz);
        }
    }

    /// 拖入遮罩：流动虚线框 + 跟随光标的柔光 + 上下轻跳的托盘图标。
    fn drag_overlay(&self, c: &Card) {
        let (x, y, w, h) = (42.0, 31.0, 335.0, 93.0);
        c.fill(x, y, w, h, 12.0, Color32::from_black_alpha(110));
        if let Some(p) = self.cursor_local {
            let px = p.x.clamp(x + 20.0, x + w - 20.0);
            let py = p.y.clamp(y + 10.0, y + h - 10.0);
            c.clipped(x, y, w, h).glow(px, py, 90.0, Color32::from_rgba_unmultiplied(120, 170, 255, 34));
        }
        c.dashed(x, y, w, h, 12.0, 1.4, Color32::from_white_alpha(230), self.t as f32 * 14.0);
        let bob = ((self.t * 5.0).sin() as f32 * 0.5 + 0.5) * 3.0;
        paint::tray(c, 209.5, 64.0, 30.0, bob, theme::WHITE, Color32::from_gray(90));
        c.text(209.5, 98.0, "松手放进置物架", 12.0, W::Semibold, theme::WHITE, H::Center);
    }

    // ================= 待办 =================
    pub(crate) fn page_todo(&mut self, c: &Card, env: &Env, _cmds: &mut Vec<Cmd>) {
        let done = self.todos.items.iter().filter(|t|t.done_on.is_some()).count();
        let total = self.todos.items.len();
        let pop = self.todo_pop.pos.abs().min(0.4);
        let hc = c.scaled(60.0, 50.0, 1.0 + pop * 0.25);
        let w = hc.text(42.0, 61.0, &format!("{done}"), size::HERO, W::Bold, if done > 0 && done == total { theme::GREEN } else { theme::WHITE }, H::Left);
        hc.text(42.0 + w, 61.0, &format!("/{total}"), size::HERO, W::Bold, theme::WHITE, H::Left);
        c.text(42.0, 78.0, "已完成", size::SMALL, W::Regular, theme::GRAY, H::Left);

        let (add, ac) = capsule(c, 41.0, 96.0, 74.0, 26.0, "t-add", theme::BTN, theme::BTN_HOVER);
        paint::plus(&ac, 56.0, 109.0, 6.2, theme::WHITE, Color32::BLACK);
        ac.text(67.0, 112.5, "加一个", size::BODY, W::Regular, theme::WHITE, H::Left);
        let can_edit = self.todos.items.iter().any(|t| t.done_on.is_none());
        let ec = if can_edit { c.clone() } else { c.faded(0.45) };
        let (edit, edc) = capsule(&ec, 121.0, 96.0, 68.0, 26.0, "t-edit", theme::BTN, if can_edit { theme::BTN_HOVER } else { theme::BTN });
        paint::pencil(&edc, 135.0, 109.0, 11.0, theme::WHITE);
        edc.text(145.0, 112.5, "改一下", size::BODY, W::Regular, theme::WHITE, H::Left);

        c.fill(196.0, 31.0, 181.0, 93.0, 12.0, theme::CARD);
        let editing = self.adding || self.editing.is_some();
        if total == 0 && !editing {
            paint::checklist(c, 286.5, 64.0, 32.0, Color32::from_gray(150), Color32::from_rgb(52, 54, 58));
            c.text(286.5, 99.0, "今天还没有安排", size::SMALL, W::Regular, theme::GRAY, H::Center);
        } else {
            let mut row = 0;
            let row_y = |r: usize| 46.0 + r as f32 * 22.0;
            if editing {
                // 输入行（文本框本身由 app::input_field 放置）
                let y = row_y(0);
                c.fill(203.0, y - 10.0, 167.0, 20.0, 7.0, Color32::from_rgb(34, 34, 38));
                c.stroke(203.0, y - 10.0, 167.0, 20.0, 7.0, 1.0, paint::alpha(theme::BLUE, 0.7));
                if self.editing.is_some() {
                    paint::pencil(c, 212.0, y, 9.0, theme::BLUE);
                } else {
                    paint::plus(c, 212.0, y, 5.0, theme::BLUE, Color32::BLACK);
                }
                row = 1;
            }
            let ctx = c.ui.ctx().clone();
            let cap = if editing { 2 } else { 3 };
            let mut shown = 0;
            for i in self.todo_first.min(total.saturating_sub(1))..total {
                if shown >= cap {
                    break;
                }
                if Some(i) == self.editing {
                    continue;
                }
                let item = self.todos.items[i].clone();
                let y = row_y(row);
                let r = c.hit(202.0, y - 10.0, 169.0, 20.0, ("todo", i, item.text.as_str()));
                let (h, _) = c.hover_t(&r);
                if h > 0.01 {
                    c.fill(202.0, y - 10.0, 169.0, 20.0, 7.0, Color32::from_white_alpha((12.0 * h) as u8));
                }
                let checked = item.done_on.is_some();
                let ct = ctx.animate_bool_with_time(egui::Id::new(("todo-check", item.text.as_str())), checked, 0.28);
                c.ring(212.0, y, 5.2, 1.3, paint::alpha(Color32::from_gray(120), 1.0 - ct));
                if ct > 0.0 {
                    c.circle(212.0, y, 6.0 * anim::ease_out_cubic(ct.min(1.0) / 0.6).min(1.0), theme::GREEN);
                    paint::check(c, 212.0, y, 6.0, smoothstep(0.3, 1.0, ct), 1.4, Color32::BLACK);
                }
                let text = c.fit(&item.text, size::BODY, W::Regular, 104.0);
                r.clone().on_hover_text(format!("{}{}",item.text,item.due.as_ref().map(|d|format!("\n提醒：{d}")).unwrap_or_default()));
                let tcol = mix(Color32::from_gray(235), Color32::from_gray(110), ct);
                let tw = c.text(223.0, y + 3.8, &text, size::BODY, W::Regular, tcol, H::Left);
                if ct > 0.0 {
                    c.fill(223.0, y + 0.2, tw * smoothstep(0.2, 1.0, ct), 1.0, 0.5, Color32::from_gray(110));
                }
                let delete=c.hit(353.0,y-9.0,17.0,18.0,("todo-delete",i)).on_hover_text("删除待办").clicked();
                paint::close(c,361.5,y,2.4,theme::GRAY);
                let reminder=c.hit(333.0,y-9.0,18.0,18.0,("todo-reminder",i)).on_hover_text(item.due.as_ref().map(|due|format!("提醒：{due} · 点击修改")).unwrap_or_else(||"设置提醒（可选）".into()));
                let reminder_clicked=reminder.clicked();
                let ink=if item.due.is_some()||reminder.hovered(){theme::BLUE}else{theme::GRAY};
                c.ring(342.0,y,4.7,1.1,ink);
                c.path(&[(342.0,y-2.5),(342.0,y),(344.3,y+1.0)],1.1,ink);
                if reminder_clicked {self.open_task_reminder(i);}
                let mut edit_this=false;
                r.context_menu(|ui|{
                    if ui.button("编辑与提醒…").clicked(){edit_this=true;ui.close_menu();}
                    if ui.button("取消提醒").clicked(){self.todos.items[i].due=None;self.todos.save();ui.close_menu();}
                });
                if edit_this {self.open_task_dialog(i);}
                if delete {self.todos.items.remove(i);self.todos.save();self.todo_first=self.todo_first.min(self.todos.items.len().saturating_sub(3));break;}
                if r.clicked() && !edit_this && !reminder_clicked {
                    let now = self.todos.items[i].done_on.is_some();
                    self.todos.items[i].done_on = if now { None } else { Some(env.today.clone()) };
                    self.todos.save();
                    if !now {
                        self.todo_pop.vel += 5.0;
                        self.pet.react(crate::extras::PetState::Happy);
                    }
                }
                row += 1;
                shown += 1;
            }
            let rest = total.saturating_sub(shown + self.editing.map(|_|1).unwrap_or(0));
            if rest > 0 {
                c.text(370.0, 120.0, &format!("共 {total} 项 · 滚轮查看"), size::TINY, W::Regular, theme::DIM, H::Right);
            }
        }

        if add.clicked() {
            self.todo_first=0;
            self.adding = true;
            self.editing = None;
            self.buf.clear();
            self.todo_input_focused=false;
        }
        if edit.clicked() && can_edit {
            if let Some(i) = self.todos.items.iter().position(|t| t.done_on.is_none()) {
                self.editing = Some(i);
                self.adding = false;
                self.buf = self.todos.items[i].text.clone();
                self.todo_input_focused=false;
            }
        }
    }

    // ---------------------------------------------------------------
    // 邮件：有验证码大卡时分栏，否则显示紧凑单行列表。
    // ---------------------------------------------------------------
    fn visible_mail_code(&self)->Option<&mail::Item> {
        self.mail.code_item().filter(|item|self.settings.mail_code && !self.mail_code_hidden(item))
    }
    pub(crate) fn mail_list_bounds(&self)->Rect {
        let left=if self.visible_mail_code().is_some(){182.0}else{36.0};
        Rect::from_min_max(Pos2::new(left,36.0),Pos2::new(380.0,122.0))
    }
    pub(crate) fn page_mail(&mut self, c: &Card, _env: &Env, cmds: &mut Vec<Cmd>) {
        if self.mail.accounts.is_empty() {
            c.text(42.0, 60.0, "还没有邮箱", 13.0, W::Bold, theme::WHITE, H::Left);
            let hint = c.fit("在设置 → 邮箱里添加；授权码只存进系统凭据管理器", 10.5, W::Regular, 344.0);
            c.text(42.0, 77.0, &hint, 10.5, W::Regular, theme::GRAY, H::Left);
            let (go, gc) = capsule(c, 42.0, 88.0, 84.0, 26.0, "mail-setup", theme::BTN, theme::BTN_HOVER);
            gc.text(58.0, 105.0, "去添加", size::BODY, W::Regular, theme::WHITE, H::Left);
            if go.clicked() {
                self.open_mail_settings();
            }
            return;
        }

        // v0.5.5：点开某封邮件 → 整块换成正文摘要，左上「返回」回到列表。
        if let Some((account, uid)) = self.mail.open_uid.clone() {
            match self
                .mail
                .recent
                .iter()
                .find(|m| m.uid == uid && m.account.eq_ignore_ascii_case(&account))
                .cloned()
            {
                Some(item) => {
                    self.mail_detail(c, &item, cmds);
                    return;
                }
                None => self.mail.open_uid = None,
            }
        }
        let code=self.visible_mail_code().cloned();
        if let Some(raw)=&code {
            c.fill(36.0,36.0,140.0,86.0,11.0,theme::CARD);
            let hidden=self.mail_code_hidden(raw);
            let item=self.display_mail(raw);
            let value=item.code.as_deref().unwrap_or("••••••");
            c.text(46.0,51.0,"验证码",10.5,W::Regular,theme::GRAY,H::Left);
            c.text(166.0,51.0,&c.fit(&item.from,10.5,W::Semibold,78.0),10.5,W::Semibold,theme::WHITE,H::Right);
            c.text(46.0,75.0,value,23.0,W::Bold,theme::GREEN,H::Left);
            c.text(46.0,91.0,&c.fit(&item.subject,10.5,W::Regular,120.0),10.5,W::Regular,Color32::from_gray(190),H::Left);
            c.text(46.0,108.0,if hidden{"已隐藏 · 点击查看"}else{"点击复制"},10.5,W::Regular,theme::DIM,H::Left);
            c.text(166.0,108.0,item.time.rsplit(' ').next().unwrap_or(&item.time),10.0,W::Regular,theme::GRAY,H::Right);
            c.hit(42.0,38.0,128.0,16.0,"mail-code-source").on_hover_text(format!("来自：{}\n{}\n收件邮箱：{}\n{}",item.from,item.subject,item.account,item.time));
            if c.hit(40.0,56.0,132.0,38.0,("mail-code",item.uid)).on_hover_text(format!("复制 {} 的验证码",item.from)).clicked(){if hidden{self.reveal_mail_code(raw);}else{cmds.push(Cmd::Copy(value.to_string()));self.code_copied(value);}}
        }
        let list_bounds=self.mail_list_bounds();
        let list_x=list_bounds.left();
        let list_w=list_bounds.width();
        let row_w=list_w-12.0;
        let compact=code.is_none();
        let row_h=if compact {21.5}else{34.0};
        if self.mail.recent.is_empty(){c.text(list_x+6.0,66.0,"还没有收到邮件",11.0,W::Regular,theme::GRAY,H::Left);return;}
        let mut content=egui::Ui::new(c.ui.ctx().clone(),egui::Id::new(("mail-list",c.live)),
            egui::UiBuilder::new().max_rect(c.rect(list_bounds.left(),list_bounds.top(),list_bounds.width(),list_bounds.height())).layer_id(c.painter.layer_id()).style(c.ui.style().clone()));
        content.set_clip_rect(c.rect(list_bounds.left(),list_bounds.top(),list_bounds.width(),list_bounds.height()).intersect(c.painter.clip_rect()));
        content.set_opacity(c.painter.opacity());if !c.live{content.disable();}
        content.spacing_mut().item_spacing.y=0.0;
        content.spacing_mut().scroll.floating=false;
        content.spacing_mut().scroll.bar_width=4.0*c.s;
        content.visuals_mut().widgets.inactive.bg_fill=Color32::from_gray(80);
        egui::ScrollArea::vertical().id_salt("mail-list-scroll").max_height(list_bounds.height()*c.s).auto_shrink([false,false]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible).show_rows(&mut content,row_h*c.s,self.mail.recent.len(),|ui,range|{
            let rows:Vec<_>=self.mail.recent[range].iter().map(|item|(self.mail_code_hidden(item),self.display_mail(item))).collect();
            for (hidden,item) in &rows {
                let (rect,_)=ui.allocate_exact_size(egui::vec2(row_w*c.s,row_h*c.s),egui::Sense::hover());
                let mut row=Card::new(ui,rect.min,ui.clip_rect());row.s=c.s;row.live=c.live;
                let hit=row.hit(0.0,0.0,row_w,row_h-1.0,("mail-row",item.uid,item.account.as_str()));
                let selected=code.as_ref().is_some_and(|m|m.uid==item.uid&&m.account.eq_ignore_ascii_case(&item.account));
                if hit.hovered()||selected{row.fill(0.0,0.0,row_w,row_h-2.0,6.0,Color32::from_gray(if hit.hovered(){27}else{17}));}
                if selected{row.fill(0.0,6.0,2.0,20.0,1.0,theme::GREEN);}
                if compact {
                    row.text(6.0,14.0,&row.fit(&item.from,11.0,W::Semibold,62.0),11.0,W::Semibold,theme::WHITE,H::Left);
                    let subject_w=row_w-118.0-if *hidden {34.0}else{0.0};
                    let width=row.text(74.0,14.0,&row.fit(&item.subject,10.5,W::Regular,subject_w),10.5,W::Regular,Color32::from_gray(200),H::Left);
                    if *hidden {
                        row.fill(78.0+width,5.0,30.0,12.0,3.0,theme::BTN);
                        row.text(93.0+width,14.0,"已隐藏",8.5,W::Regular,theme::GRAY,H::Center);
                    }
                    row.text(row_w-6.0,14.0,item.time.rsplit(' ').next().unwrap_or(&item.time),10.0,W::Regular,Color32::from_gray(170),H::Right);
                    row.seg((6.0,row_h-0.5),(row_w-6.0,row_h-0.5),0.5,Color32::from_gray(40));
                } else {
                row.text(6.0,12.0,&row.fit(&item.from,11.5,W::Semibold,row_w-54.0),11.5,W::Semibold,theme::WHITE,H::Left);
                row.text(row_w-6.0,12.0,item.time.rsplit(' ').next().unwrap_or(&item.time),10.0,W::Regular,Color32::from_gray(170),H::Right);
                row.text(6.0,27.0,&row.fit(&item.subject,10.5,W::Regular,row_w-12.0),10.5,W::Regular,Color32::from_gray(200),H::Left);
                }
                hit.clone().on_hover_text(format!("{}\n{}\n收件邮箱：{}\n{}",item.from,item.subject,item.account,item.time));
                if hit.clicked(){self.mail.open_uid=Some((item.account.clone(),item.uid));}
            }
        });
    }

    /// 岛内邮件详情可滚动，也可以进入独立窗口阅读全文。
    pub(crate) fn mail_detail(&mut self, c: &Card, raw: &mail::Item, cmds: &mut Vec<Cmd>) {
        let hidden=self.mail_code_hidden(raw);
        let display=self.display_mail(raw);let item=&display;
        c.fill(36.0, 36.0, 347.0, 188.0, 11.0, theme::CARD);
        let back = c.hit(330.0, 42.0, 44.0, 14.0, ("mail-back", item.uid));
        let bh = c.hover_t(&back).0;
        c.text(374.0, 50.0, "返回", 10.5, W::Regular, if bh > 0.5 { theme::WHITE } else { theme::GRAY }, H::Right);
        if back.clicked() {
            self.mail.open_uid = None;
            return;
        }
        let from = c.fit(&item.from, 11.5, W::Semibold, 210.0);
        c.text(46.0, 50.0, &from, 11.5, W::Semibold, theme::WHITE, H::Left);
        let read=c.hit(266.0,42.0,60.0,14.0,("mail-read",&item.account,item.uid)).on_hover_text("在独立窗口阅读完整正文");
        c.text(324.0,50.0,"阅读全文",10.5,W::Regular,if read.hovered(){theme::BLUE}else{theme::GRAY},H::Right);
        if read.clicked(){self.mail.reading=Some(raw.clone());self.mail.reader_focus=true;}
        let subject = c.fit(&item.subject, 13.5, W::Semibold, 328.0);
        c.text(46.0, 73.0, &subject, 13.5, W::Semibold, theme::WHITE, H::Left);
        c.hit(46.0,58.0,328.0,20.0,("mail-subject",&item.account,item.uid)).on_hover_text(&item.subject);
        c.text(374.0, 92.0, &item.time, 10.0, W::Regular, Color32::from_gray(170), H::Right);
        let account=format!("收件邮箱：{}",item.account);
        c.text(46.0,92.0,&c.fit(&account,10.0,W::Regular,230.0),10.0,W::Regular,Color32::from_gray(175),H::Left);
        c.hit(46.0,80.0,230.0,16.0,("mail-account",&item.account,item.uid)).on_hover_text(&account);

        // 正文和链接共用滚动区域，会议与验证码操作保留在底部。
        let mut content=egui::Ui::new(c.ui.ctx().clone(),egui::Id::new(("mail-content",&item.account,item.uid,c.live)),
            egui::UiBuilder::new().max_rect(c.rect(46.0,104.0,328.0,90.0)).layer_id(c.painter.layer_id()).style(c.ui.style().clone()));
        content.set_clip_rect(c.rect(46.0,104.0,328.0,90.0).intersect(c.painter.clip_rect()));
        content.set_opacity(c.painter.opacity());
        if !c.live {content.disable();}
        content.spacing_mut().item_spacing.y=6.0*c.s;
        content.style_mut().spacing.scroll.floating=false;
        content.spacing_mut().scroll.bar_width=4.0*c.s;
        egui::ScrollArea::vertical().id_salt(("mail-body",&item.account,item.uid)).max_height(90.0*c.s).auto_shrink([false,false]).show(&mut content,|ui|{
            if !item.summary.is_empty() {
                ui.add(egui::Label::new(egui::RichText::new(item.summary.as_ref()).font(c.font(12.0,W::Regular)).color(Color32::from_gray(220))).wrap());
            }else if item.links.is_empty(){
                ui.label(egui::RichText::new("（这封邮件没有正文）").font(c.font(12.0,W::Regular)).color(theme::DIM));
            }
            for link in &item.links {
                let text=format!("{} ↗",c.fit(&link.label,12.0,W::Regular,295.0));
                if ui.add(egui::Link::new(egui::RichText::new(text).font(c.font(12.0,W::Regular)).color(theme::BLUE))).on_hover_text(&link.url).clicked(){
                    cmds.push(Cmd::OpenUrl(link.url.clone()));
                }
            }
        });

        // 底部一行：会议时间（绿）优先，其次验证码（点一下复制）。
        c.seg((46.0,199.0),(374.0,199.0),0.6,Color32::from_gray(48));
        let y = 214.0;
        if hidden {
            c.text(46.0,y,"验证码已隐藏（不代表失效）",11.0,W::Regular,theme::GRAY,H::Left);
            c.text(374.0,y,"查看 15 秒",10.5,W::Regular,theme::BLUE,H::Right);
            if c.hit(290.0,y-14.0,86.0,24.0,("mail-reveal",item.uid)).clicked(){self.reveal_mail_code(raw);}
        } else if let Some(m) = &item.meeting {
            let ignored=self.mail_event_ignored(m);
            c.text(46.0,y,&format!("{} · {}",if ignored{"已忽略"}else{"日程"},m.at),10.5,W::Regular,if ignored{theme::DIM}else{theme::GREEN},H::Left);
            let label=if ignored{"恢复日程"}else{"忽略日程"};
            c.text(374.0,y,label,10.5,W::Regular,theme::GRAY,H::Right);
            if c.hit(316.0,y-14.0,60.0,24.0,("mail-ignore-event",item.uid)).on_hover_text(&m.title).clicked(){self.toggle_mail_event(m);}
        } else if let Some(code) = &item.code {
            let label = format!("验证码 {code}");
            let w = c.text(46.0, y, &label, 13.0, W::Semibold, theme::GREEN, H::Left);
            c.text(46.0 + w + 6.0, y, "点击复制", 10.5, W::Regular, theme::DIM, H::Left);
            let hit = c.hit(42.0, y - 14.0, w + 60.0, 24.0, ("mail-detail-code", item.uid));
            if hit.clicked() {
                cmds.push(Cmd::Copy(code.clone()));
                self.code_copied(code);
            }
        }
    }
}
