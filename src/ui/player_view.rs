//! The video surface with its overlay controls.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};

use super::format_time;
use crate::image;
use crate::playback::Playback;
use crate::player::PlayState;
use crate::settings;

const HIDE_DELAY: Duration = Duration::from_millis(2800);
const SPEEDS: [f64; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0];

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PlayerView {
        pub playback: RefCell<Option<Playback>>,
        pub overlay: gtk::Overlay,
        pub picture: gtk::Picture,
        pub poster: gtk::Picture,
        pub spinner: adw::Spinner,
        pub error_box: gtk::Box,
        pub error_label: gtk::Label,
        pub controls: gtk::Revealer,
        pub title: gtk::Label,
        pub collapse_button: gtk::Button,
        pub play_button: gtk::Button,
        pub prev_button: gtk::Button,
        pub next_button: gtk::Button,
        pub scale: gtk::Scale,
        pub time_label: gtk::Label,
        pub fullscreen_button: gtk::Button,
        pub mute_button: gtk::Button,
        pub settings_button: gtk::MenuButton,
        pub settings_box: gtk::Box,
        pub feedback: gtk::Label,
        pub stats: gtk::Label,
        pub stats_visible: Cell<bool>,
        pub hide_source: RefCell<Option<glib::SourceId>>,
        pub feedback_source: RefCell<Option<glib::SourceId>>,
        pub click_source: RefCell<Option<glib::SourceId>>,
        pub seek_source: RefCell<Option<glib::SourceId>>,
        pub user_seeking: Cell<bool>,
        pub last_preview_seek: Cell<Option<Instant>>,
        pub fullscreen: Cell<bool>,
        pub pointer_over_controls: Cell<bool>,
        pub last_motion: Cell<(f64, f64)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlayerView {
        const NAME: &'static str = "SlopPlayerView";
        type Type = super::PlayerView;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for PlayerView {
        fn dispose(&self) {
            for s in [&self.hide_source, &self.feedback_source, &self.click_source, &self.seek_source] {
                if let Some(id) = s.take() {
                    id.remove();
                }
            }
        }
    }
    impl WidgetImpl for PlayerView {}
    impl BinImpl for PlayerView {}
}

glib::wrapper! {
    pub struct PlayerView(ObjectSubclass<imp::PlayerView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tooltip));
    b.add_css_class("osd-button");
    b.add_css_class("flat");
    b.set_valign(gtk::Align::Center);
    b
}

impl PlayerView {
    pub fn new(playback: &Playback) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().playback.replace(Some(playback.clone()));
        obj.build();
        obj.bind_playback();
        obj
    }

    fn playback(&self) -> Playback {
        self.imp().playback.borrow().clone().unwrap()
    }

    fn build(&self) {
        let imp = self.imp();
        self.add_css_class("player-view");
        self.set_focusable(true);

        // Video
        imp.picture.set_content_fit(gtk::ContentFit::Contain);
        imp.picture.set_can_shrink(true);
        imp.picture
            .set_paintable(self.playback().player().paintable().as_ref());
        let offload = gtk::GraphicsOffload::new(Some(&imp.picture));
        offload.set_black_background(true);
        offload.set_hexpand(true);
        offload.set_vexpand(true);
        imp.overlay.set_child(Some(&offload));

        imp.poster.set_content_fit(gtk::ContentFit::Contain);
        imp.poster.set_can_shrink(true);
        imp.poster.add_css_class("poster");
        imp.poster.set_can_target(false);
        imp.overlay.add_overlay(&imp.poster);

        imp.spinner.set_size_request(48, 48);
        imp.spinner.set_halign(gtk::Align::Center);
        imp.spinner.set_valign(gtk::Align::Center);
        imp.spinner.set_can_target(false);
        imp.spinner.add_css_class("player-spinner");
        imp.overlay.add_overlay(&imp.spinner);

        // Error
        imp.error_box.set_orientation(gtk::Orientation::Vertical);
        imp.error_box.set_spacing(12);
        imp.error_box.set_halign(gtk::Align::Center);
        imp.error_box.set_valign(gtk::Align::Center);
        imp.error_box.add_css_class("player-error");
        let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
        icon.set_pixel_size(32);
        imp.error_label.set_wrap(true);
        imp.error_label.set_justify(gtk::Justification::Center);
        imp.error_label.set_max_width_chars(48);
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("pill");
        retry.set_halign(gtk::Align::Center);
        retry.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.playback().retry()
        ));
        imp.error_box.append(&icon);
        imp.error_box.append(&imp.error_label);
        imp.error_box.append(&retry);
        imp.error_box.set_visible(false);
        imp.overlay.add_overlay(&imp.error_box);

        // Controls
        let controls = gtk::Box::new(gtk::Orientation::Vertical, 0);
        controls.add_css_class("player-controls");

        let top = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        top.add_css_class("player-top");
        imp.collapse_button.set_icon_name("go-down-symbolic");
        imp.collapse_button.set_tooltip_text(Some("Minimize"));
        imp.collapse_button.add_css_class("osd-button");
        imp.collapse_button.add_css_class("flat");
        imp.collapse_button.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                if view.imp().fullscreen.get() {
                    let _ = view.activate_action("win.toggle-fullscreen", None);
                } else {
                    let _ = view.activate_action("win.collapse-player", None);
                }
            }
        ));
        top.append(&imp.collapse_button);
        imp.title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.title.set_hexpand(true);
        imp.title.set_xalign(0.0);
        imp.title.add_css_class("player-title");
        top.append(&imp.title);
        imp.settings_button.set_icon_name("emblem-system-symbolic");
        imp.settings_button.set_tooltip_text(Some("Settings"));
        imp.settings_button.add_css_class("osd-button");
        imp.settings_button.add_css_class("flat");
        let popover = gtk::Popover::new();
        imp.settings_box.set_orientation(gtk::Orientation::Vertical);
        imp.settings_box.set_spacing(6);
        imp.settings_box.add_css_class("player-settings");
        let settings_scroll = gtk::ScrolledWindow::builder()
            .child(&imp.settings_box)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(420)
            .build();
        popover.set_child(Some(&settings_scroll));
        popover.connect_show(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.rebuild_settings()
        ));
        imp.settings_button.set_popover(Some(&popover));
        // Keeps the buttons on the right while the title (fullscreen only) is hidden.
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        top.append(&spacer);
        top.append(&imp.settings_button);
        // The watch view covers the header bar, so keep the window buttons reachable.
        let window_controls = gtk::WindowControls::new(gtk::PackType::End);
        window_controls.add_css_class("osd-window-controls");
        top.append(&window_controls);
        let handle = gtk::WindowHandle::new();
        handle.set_child(Some(&top));
        controls.append(&handle);

        let center = gtk::Box::new(gtk::Orientation::Horizontal, 32);
        center.set_halign(gtk::Align::Center);
        center.set_valign(gtk::Align::Center);
        center.set_vexpand(true);
        imp.prev_button.set_icon_name("media-skip-backward-symbolic");
        imp.prev_button.set_tooltip_text(Some("Previous"));
        let rewind = icon_button("media-seek-backward-symbolic", "Back 10 seconds (J)");
        rewind.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.seek_by(-10.0)
        ));
        imp.play_button.set_icon_name("media-playback-start-symbolic");
        imp.play_button.add_css_class("osd-button");
        imp.play_button.add_css_class("play-button");
        imp.play_button.add_css_class("circular");
        imp.play_button.set_tooltip_text(Some("Play/Pause (K)"));
        imp.play_button.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                view.playback().player().toggle();
                view.schedule_hide();
            }
        ));
        let forward = icon_button("media-seek-forward-symbolic", "Forward 10 seconds (L)");
        forward.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.seek_by(10.0)
        ));
        center.append(&rewind);
        center.append(&imp.play_button);
        center.append(&forward);
        controls.append(&center);

        let bottom = gtk::Box::new(gtk::Orientation::Vertical, 0);
        bottom.add_css_class("player-bottom");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        imp.time_label.add_css_class("player-time");
        imp.time_label.add_css_class("numeric");
        imp.time_label.set_hexpand(true);
        imp.time_label.set_xalign(0.0);
        imp.time_label.set_text("0:00 / 0:00");
        row.append(&imp.time_label);
        imp.next_button.set_icon_name("media-skip-forward-symbolic");
        imp.next_button.set_tooltip_text(Some("Next (Shift+N)"));
        imp.next_button.add_css_class("osd-button");
        imp.next_button.add_css_class("flat");
        imp.next_button.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                view.playback().next();
            }
        ));
        row.append(&imp.next_button);
        imp.mute_button.set_icon_name("audio-volume-high-symbolic");
        imp.mute_button.set_tooltip_text(Some("Mute (M)"));
        imp.mute_button.add_css_class("osd-button");
        imp.mute_button.add_css_class("flat");
        imp.mute_button.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                let p = view.playback().player();
                p.set_muted(!p.muted());
            }
        ));
        row.append(&imp.mute_button);
        imp.fullscreen_button.set_icon_name("view-fullscreen-symbolic");
        imp.fullscreen_button.set_tooltip_text(Some("Fullscreen (F)"));
        imp.fullscreen_button.add_css_class("osd-button");
        imp.fullscreen_button.add_css_class("flat");
        imp.fullscreen_button.set_action_name(Some("win.toggle-fullscreen"));
        row.append(&imp.fullscreen_button);
        bottom.append(&row);

        imp.scale.set_orientation(gtk::Orientation::Horizontal);
        imp.scale.set_adjustment(&gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 10.0, 0.0));
        imp.scale.set_draw_value(false);
        imp.scale.add_css_class("seek-bar");
        imp.scale.set_hexpand(true);
        imp.scale.connect_change_value(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, _, value| {
                view.user_seek(value);
                glib::Propagation::Proceed
            }
        ));
        bottom.append(&imp.scale);
        controls.append(&bottom);

        imp.controls.set_child(Some(&controls));
        imp.controls.set_transition_type(gtk::RevealerTransitionType::Crossfade);
        imp.controls.set_transition_duration(180);
        imp.controls.set_reveal_child(true);
        let motion_controls = gtk::EventControllerMotion::new();
        motion_controls.connect_enter(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _, _| view.imp().pointer_over_controls.set(true)
        ));
        motion_controls.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.imp().pointer_over_controls.set(false)
        ));
        top.add_controller(motion_controls.clone());
        let motion_bottom = gtk::EventControllerMotion::new();
        motion_bottom.connect_enter(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _, _| view.imp().pointer_over_controls.set(true)
        ));
        motion_bottom.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.imp().pointer_over_controls.set(false)
        ));
        bottom.add_controller(motion_bottom);
        imp.overlay.add_overlay(&imp.controls);

        imp.feedback.add_css_class("seek-feedback");
        imp.feedback.set_halign(gtk::Align::Center);
        imp.feedback.set_valign(gtk::Align::Center);
        imp.feedback.set_can_target(false);
        imp.feedback.set_visible(false);
        imp.overlay.add_overlay(&imp.feedback);

        imp.stats.add_css_class("stats-overlay");
        imp.stats.add_css_class("monospace");
        imp.stats.set_halign(gtk::Align::Start);
        imp.stats.set_valign(gtk::Align::Start);
        imp.stats.set_xalign(0.0);
        imp.stats.set_can_target(false);
        imp.stats.set_visible(false);
        imp.overlay.add_overlay(&imp.stats);

        self.set_child(Some(&imp.overlay));
        self.setup_gestures();
        self.set_fullscreen(false);
    }

    fn setup_gestures(&self) {
        let imp = self.imp();
        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, x, y| {
                // Ignore synthetic motion events with no movement (e.g. from relayouts).
                let (lx, ly) = view.imp().last_motion.get();
                if (lx - x).abs() < 1.0 && (ly - y).abs() < 1.0 {
                    return;
                }
                view.imp().last_motion.set((x, y));
                view.show_controls();
            }
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.schedule_hide_soon()
        ));
        self.add_controller(motion);

        // Released (not pressed): a press that turns into a drag of the sheet
        // cancels this gesture, so dragging never toggles playback.
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_PRIMARY);
        click.connect_released(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |g, n_press, x, _| {
                let touch = g
                    .current_event_device()
                    .is_some_and(|d| d.source() == gdk::InputSource::Touchscreen);
                view.on_video_click(n_press, x, touch);
            }
        ));
        imp.overlay.add_controller(click);
    }

    /// A press became a drag: forget the pending single-click action.
    pub fn cancel_pending_click(&self) {
        if let Some(s) = self.imp().click_source.take() {
            s.remove();
        }
    }

    /// What the player currently shows (video frame or poster), for transitions.
    pub fn visual_paintable(&self) -> Option<gdk::Paintable> {
        let imp = self.imp();
        if imp.poster.is_visible() || !imp.picture.is_visible() {
            imp.poster.paintable().or_else(|| imp.picture.paintable())
        } else {
            imp.picture.paintable()
        }
    }

    fn on_video_click(&self, n_press: i32, x: f64, touch: bool) {
        let imp = self.imp();
        if let Some(s) = imp.click_source.take() {
            s.remove();
        }
        let width = self.width().max(1) as f64;
        if n_press >= 2 {
            if touch {
                if x < width / 3.0 {
                    self.seek_by(-10.0);
                } else if x > width * 2.0 / 3.0 {
                    self.seek_by(10.0);
                } else {
                    self.playback().player().toggle();
                }
            } else {
                let _ = self.activate_action("win.toggle-fullscreen", None);
            }
            return;
        }
        // Wait to see whether this becomes a double click.
        let delay = self
            .settings()
            .gtk_double_click_time()
            .clamp(150, 400) as u64;
        let id = glib::timeout_add_local_once(
            Duration::from_millis(delay),
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    view.imp().click_source.take();
                    if touch {
                        if view.imp().controls.reveals_child() {
                            view.hide_controls();
                        } else {
                            view.show_controls();
                        }
                    } else {
                        view.playback().player().toggle();
                        view.show_controls();
                    }
                }
            ),
        );
        imp.click_source.replace(Some(id));
    }

    fn bind_playback(&self) {
        let playback = self.playback();
        let player = playback.player();

        player.connect_position_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.update_time()
        ));
        player.connect_duration_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |p| {
                let adj = view.imp().scale.adjustment();
                adj.set_upper(p.duration().max(1.0));
                view.update_time();
            }
        ));
        player.connect_state_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.update_state()
        ));
        player.connect_muted_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |p| {
                view.imp().mute_button.set_icon_name(if p.muted() {
                    "audio-volume-muted-symbolic"
                } else {
                    "audio-volume-high-symbolic"
                });
            }
        ));
        for prop in ["loading-streams", "error", "audio-only"] {
            playback.connect_notify_local(
                Some(prop),
                glib::clone!(
                    #[weak(rename_to = view)]
                    self,
                    move |_, _| view.update_state()
                ),
            );
        }
        playback.connect_title_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |pb| view.imp().title.set_text(&pb.title())
        ));
        playback.connect_thumbnail_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |pb| image::set_picture(&view.imp().poster, Some(&pb.thumbnail()), 1280)
        ));
        playback.connect_local(
            "video-changed",
            false,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                #[upgrade_or_default]
                move |_| {
                    view.imp().scale.adjustment().set_value(0.0);
                    view.show_controls();
                    view.update_state();
                    None
                }
            ),
        );
        for sig in ["details-loaded", "streams-loaded"] {
            playback.connect_local(
                sig,
                false,
                glib::clone!(
                    #[weak(rename_to = view)]
                    self,
                    #[upgrade_or_default]
                    move |_| {
                        view.update_state();
                        None
                    }
                ),
            );
        }
        // Stats refresh
        glib::timeout_add_local(
            Duration::from_millis(1000),
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    if view.imp().stats_visible.get() && view.is_mapped() {
                        view.update_stats();
                    }
                    glib::ControlFlow::Continue
                }
            ),
        );
        self.update_state();
    }

    fn update_time(&self) {
        let imp = self.imp();
        let player = self.playback().player();
        let pos = player.position();
        let dur = player.duration();
        if !imp.user_seeking.get() {
            imp.scale.adjustment().set_value(pos);
        }
        let shown = if imp.user_seeking.get() { imp.scale.value() } else { pos };
        if player.is_live() {
            imp.time_label.set_markup("<span foreground=\"#ff4e45\">●</span> LIVE");
        } else {
            imp.time_label
                .set_text(&format!("{} / {}", format_time(shown), format_time(dur)));
        }
    }

    pub fn update_state(&self) {
        let imp = self.imp();
        let playback = self.playback();
        let player = playback.player();
        let state = player.state();
        let error = playback.error();
        let loading = playback.loading_streams() || matches!(state, PlayState::Loading | PlayState::Buffering);
        let has_error = !error.is_empty();

        imp.spinner.set_visible(loading && !has_error);
        imp.error_box.set_visible(has_error);
        imp.error_label.set_text(&error);
        let show_poster = has_error
            || playback.audio_only()
            || playback.loading_streams()
            || matches!(state, PlayState::Idle | PlayState::Loading | PlayState::Error);
        imp.poster.set_visible(show_poster);
        imp.picture.set_visible(!playback.audio_only());

        let playing = player.is_playing_intent() && state != PlayState::Ended;
        imp.play_button.set_icon_name(match state {
            PlayState::Ended => "view-refresh-symbolic",
            _ if playing => "media-playback-pause-symbolic",
            _ => "media-playback-start-symbolic",
        });
        imp.play_button.set_visible(!loading || state == PlayState::Buffering && !playing);
        imp.next_button.set_visible(playback.has_next());
        imp.scale.set_sensitive(!player.is_live() && player.duration() > 0.0);
        if !playing || state == PlayState::Ended {
            self.show_controls_persistent();
        } else {
            self.schedule_hide();
        }
    }

    // -----------------------------------------------------------------------
    // Controls visibility
    // -----------------------------------------------------------------------

    pub fn show_controls(&self) {
        let imp = self.imp();
        imp.controls.set_reveal_child(true);
        self.set_cursor(None);
        self.schedule_hide();
    }

    fn show_controls_persistent(&self) {
        let imp = self.imp();
        if let Some(s) = imp.hide_source.take() {
            s.remove();
        }
        imp.controls.set_reveal_child(true);
        self.set_cursor(None);
    }

    fn hide_controls(&self) {
        let imp = self.imp();
        if let Some(s) = imp.hide_source.take() {
            s.remove();
        }
        let player = self.playback().player();
        if !player.is_playing_intent() || imp.settings_button.is_active() {
            return;
        }
        imp.controls.set_reveal_child(false);
        if self.is_mapped() {
            self.set_cursor_from_name(Some("none"));
        }
    }

    fn schedule_hide_with(&self, delay: Duration) {
        let imp = self.imp();
        if let Some(s) = imp.hide_source.take() {
            s.remove();
        }
        let id = glib::timeout_add_local_once(
            delay,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    view.imp().hide_source.take();
                    if view.imp().pointer_over_controls.get() {
                        view.schedule_hide();
                    } else {
                        view.hide_controls();
                    }
                }
            ),
        );
        imp.hide_source.replace(Some(id));
    }

    fn schedule_hide(&self) {
        self.schedule_hide_with(HIDE_DELAY);
    }

    fn schedule_hide_soon(&self) {
        self.imp().pointer_over_controls.set(false);
        self.schedule_hide_with(Duration::from_millis(600));
    }

    // -----------------------------------------------------------------------
    // Seeking
    // -----------------------------------------------------------------------

    pub fn seek_by(&self, delta: f64) {
        let player = self.playback().player();
        player.seek_relative(delta);
        let imp = self.imp();
        imp.feedback
            .set_text(&if delta < 0.0 { format!("« {} s", -delta as i64) } else { format!("{} s »", delta as i64) });
        imp.feedback.set_halign(if delta < 0.0 { gtk::Align::Start } else { gtk::Align::End });
        imp.feedback.set_visible(true);
        if let Some(s) = imp.feedback_source.take() {
            s.remove();
        }
        let id = glib::timeout_add_local_once(
            Duration::from_millis(700),
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    view.imp().feedback_source.take();
                    view.imp().feedback.set_visible(false);
                }
            ),
        );
        imp.feedback_source.replace(Some(id));
        self.show_controls();
    }

    fn user_seek(&self, value: f64) {
        let imp = self.imp();
        imp.user_seeking.set(true);
        let player = self.playback().player();
        let now = Instant::now();
        let due = imp
            .last_preview_seek
            .get()
            .is_none_or(|t| now.duration_since(t) > Duration::from_millis(150));
        if due {
            imp.last_preview_seek.set(Some(now));
            player.seek(value, false);
        }
        self.update_time();
        if let Some(s) = imp.seek_source.take() {
            s.remove();
        }
        let id = glib::timeout_add_local_once(
            Duration::from_millis(280),
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || {
                    let imp = view.imp();
                    imp.seek_source.take();
                    let v = imp.scale.value();
                    view.playback().player().seek(v, true);
                    imp.user_seeking.set(false);
                    imp.last_preview_seek.set(None);
                }
            ),
        );
        imp.seek_source.replace(Some(id));
        self.show_controls();
    }

    // -----------------------------------------------------------------------
    // Fullscreen, settings, stats
    // -----------------------------------------------------------------------

    pub fn set_fullscreen(&self, fullscreen: bool) {
        let imp = self.imp();
        imp.fullscreen.set(fullscreen);
        imp.title.set_visible(fullscreen);
        imp.collapse_button.set_icon_name(if fullscreen { "go-previous-symbolic" } else { "go-down-symbolic" });
        imp.collapse_button
            .set_tooltip_text(Some(if fullscreen { "Exit fullscreen" } else { "Minimize" }));
        imp.fullscreen_button.set_icon_name(if fullscreen {
            "view-restore-symbolic"
        } else {
            "view-fullscreen-symbolic"
        });
        if fullscreen {
            self.add_css_class("fullscreen");
        } else {
            self.remove_css_class("fullscreen");
        }
    }

    pub fn toggle_stats(&self) {
        let imp = self.imp();
        let v = !imp.stats_visible.get();
        imp.stats_visible.set(v);
        imp.stats.set_visible(v);
        if v {
            self.update_stats();
        }
    }

    fn update_stats(&self) {
        let imp = self.imp();
        let playback = self.playback();
        let player = playback.player();
        let st = player.stats();
        let mut lines = Vec::new();
        if let Some(sel) = playback.selection() {
            if let Some(v) = sel.video_format() {
                lines.push(format!(
                    "Video     {} {}p{} itag {} ({} kbps)",
                    v.vcodec.as_deref().unwrap_or("?"),
                    v.height.unwrap_or(0),
                    v.fps.map(|f| f.round() as u32).unwrap_or(0),
                    v.format_id,
                    v.tbr.unwrap_or(0.0).round()
                ));
            }
            if let Some(a) = sel.audio_format() {
                lines.push(format!(
                    "Audio     {} itag {} ({} kbps)",
                    a.acodec.as_deref().unwrap_or("?"),
                    a.format_id,
                    a.abr.or(a.tbr).unwrap_or(0.0).round()
                ));
            }
        }
        lines.push(format!(
            "Decoder   {}{}",
            st.video_decoder.as_deref().unwrap_or("-"),
            if st.hardware { " (hardware)" } else { " (software)" }
        ));
        lines.push(format!("Audio dec {}", st.audio_decoder.as_deref().unwrap_or("-")));
        lines.push(format!(
            "Frames    {}×{} {} {} fps",
            st.width, st.height, st.format, st.framerate
        ));
        lines.push(format!(
            "Path      {}",
            if st.dmabuf { "DMABuf zero-copy" } else { "system memory upload" }
        ));
        lines.push(format!("Rendered  {}  dropped {}", st.rendered, st.dropped));
        lines.push(format!("Buffer    {}%  speed {}×", player.buffering(), player.rate()));
        imp.stats.set_text(&lines.join("\n"));
    }

    fn rebuild_settings(&self) {
        let imp = self.imp();
        let b = &imp.settings_box;
        while let Some(c) = b.first_child() {
            b.remove(&c);
        }
        let playback = self.playback();
        let header = |t: &str| {
            let l = gtk::Label::new(Some(t));
            l.set_xalign(0.0);
            l.add_css_class("heading");
            l.set_margin_top(4);
            l
        };

        b.append(&header("Quality"));
        let quality_box = gtk::FlowBox::new();
        quality_box.set_selection_mode(gtk::SelectionMode::None);
        quality_box.set_max_children_per_line(4);
        quality_box.set_min_children_per_line(3);
        quality_box.set_column_spacing(4);
        quality_box.set_row_spacing(4);
        let current = playback.quality_override();
        let active_height = playback
            .selection()
            .and_then(|s| s.video_format().and_then(|v| v.height));
        let auto = gtk::ToggleButton::with_label(&match (current, active_height) {
            (None, Some(h)) => format!("Auto ({h}p)"),
            _ => "Auto".to_string(),
        });
        auto.set_active(current.is_none());
        auto.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                view.playback().set_quality(None);
                view.imp().settings_button.popdown();
            }
        ));
        quality_box.append(&auto);
        for opt in playback.quality_options() {
            let label = format!(
                "{}{}",
                opt.label(),
                if opt.hardware { "" } else { " ⚠" }
            );
            let t = gtk::ToggleButton::with_label(&label);
            t.set_group(Some(&auto));
            t.set_tooltip_text(Some(&format!(
                "{} — {} decoding",
                opt.codec.label(),
                if opt.hardware { "hardware" } else { "software" }
            )));
            t.set_active(current == Some(opt.height));
            let h = opt.height;
            t.connect_clicked(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |btn| {
                    if btn.is_active() {
                        view.playback().set_quality(Some(h));
                        view.imp().settings_button.popdown();
                    }
                }
            ));
            quality_box.append(&t);
        }
        b.append(&quality_box);

        b.append(&header("Speed"));
        let speed_box = gtk::FlowBox::new();
        speed_box.set_selection_mode(gtk::SelectionMode::None);
        speed_box.set_max_children_per_line(4);
        speed_box.set_min_children_per_line(4);
        speed_box.set_column_spacing(4);
        speed_box.set_row_spacing(4);
        let player = playback.player();
        let mut first: Option<gtk::ToggleButton> = None;
        for s in SPEEDS {
            let t = gtk::ToggleButton::with_label(&format!("{s}×"));
            if let Some(f) = &first {
                t.set_group(Some(f));
            } else {
                first = Some(t.clone());
            }
            t.set_active((player.rate() - s).abs() < 0.01);
            t.connect_clicked(glib::clone!(
                #[weak]
                player,
                move |btn| {
                    if btn.is_active() {
                        player.set_rate(s);
                        settings::update(|st| st.speed = s);
                    }
                }
            ));
            speed_box.append(&t);
        }
        b.append(&speed_box);

        let audio = gtk::CheckButton::with_label("Audio only");
        audio.set_active(playback.audio_only());
        audio.connect_toggled(glib::clone!(
            #[weak]
            playback,
            move |c| playback.set_audio_only(c.is_active())
        ));
        b.append(&audio);
        let stats = gtk::CheckButton::with_label("Stats for nerds");
        stats.set_active(imp.stats_visible.get());
        stats.connect_toggled(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.toggle_stats()
        ));
        b.append(&stats);
        let vol_label = header("Volume");
        b.append(&vol_label);
        let vol = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.05);
        vol.set_value(player.volume());
        vol.set_draw_value(false);
        vol.connect_value_changed(glib::clone!(
            #[weak]
            player,
            move |s| player.set_volume(s.value())
        ));
        b.append(&vol);
    }

    pub fn grab_player_focus(&self) {
        self.grab_focus();
    }
}
