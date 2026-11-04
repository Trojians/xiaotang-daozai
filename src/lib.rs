//! 岛仔：Windows 顶部「灵动岛」。
//!
//! - `app` / `pages`：与平台无关的界面与动效（外壳与录制器共用）
//! - `anim` / `paint` / `shape` / `theme`：弹簧、矢量绘制、面板轮廓、配色字体
//! - `model` / `lyrics` / `smtc` / `icons`：数据、歌词、系统媒体、文件图标
//! - `native` / `tray`：仅 Windows 的窗口区域、命中穿透与托盘

pub mod display_layout;
mod mail_privacy;
pub mod poll_clock;
pub mod anim;
pub mod app;
pub mod bt;
pub mod calendar;
mod calendar_ui;
mod local_calendar;
mod calendar_editor;
pub mod glass;
pub mod icons;
pub mod input;
pub mod lyrics;
pub mod mail;
mod mail_reader;
mod task_picker;
pub mod model;
pub mod otp;
pub mod quota;
mod quota_ui;
mod pages;
mod extras;
mod pet;
mod bar_layout;
mod resident_settings;
mod avoidance;
mod focus_bar;
#[cfg(windows)]
pub mod system;
pub mod paint;
pub mod shape;
pub mod smtc;
pub mod theme;
pub mod settings_theme;
mod updates;

#[cfg(windows)]
pub mod native;
#[cfg(windows)]
pub mod tray;

#[cfg(test)]
mod ui_tests;
#[cfg(test)]
#[path = "bin/daozai-record/raster.rs"]
mod preview_raster;
