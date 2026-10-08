//! The watch page: player on top, details/comments below, related videos beside or below.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::aspect::AspectBin;
use super::card::{CardStyle, ItemCard};
use super::comments::CommentsPage;
use super::player_view::PlayerView;
use super::{Nav, navigate};
use crate::api::{self, Item, LikeStatus, VideoDetails};
use crate::image;
use crate::playback::Playback;
use crate::runtime;
use crate::settings;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct WatchView {
        pub playback: RefCell<Option<Playback>>,
        pub player_view: RefCell<Option<PlayerView>>,
        pub player_frame: RefCell<Option<AspectBin>>,
        pub columns: gtk::Box,
        pub main_col: gtk::Box,
        pub nav: adw::NavigationView,
        pub details_scroll: gtk::ScrolledWindow,
        pub details_box: gtk::Box,
        pub side_scroll: gtk::ScrolledWindow,
        pub side_box: gtk::Box,
        pub title: gtk::Label,
        pub meta: gtk::Label,
        pub channel_avatar: adw::Avatar,
        pub channel_name: gtk::Label,
        pub channel_verified: gtk::Image,
        pub channel_subs: gtk::Label,
        pub subscribe: gtk::Button,
        pub like: gtk::ToggleButton,
        pub like_label: gtk::Label,
        pub dislike: gtk::ToggleButton,
        pub description: gtk::Label,
        pub description_card: gtk::Box,
        pub description_expanded: Cell<bool>,
        pub comments_card: gtk::Button,
        pub comments_title: gtk::Label,
        pub comments_preview: gtk::Label,
        pub queue_box: gtk::Box,
        pub queue_title: gtk::Label,
        pub queue_list: gtk::Box,
        pub related_header: gtk::Box,
        pub related_list: gtk::Box,
        pub related_more: gtk::Button,
        pub details_spinner: adw::Spinner,
        pub related_continuation: RefCell<Option<String>>,
        pub wide: Cell<bool>,
        pub fullscreen: Cell<bool>,
        pub updating: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for WatchView {
        const NAME: &'static str = "SlopWatchView";
        type Type = super::WatchView;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for WatchView {}

    impl WidgetImpl for WatchView {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(frame) = self.player_frame.borrow().as_ref() {
                let max = if self.fullscreen.get() {
                    height
                } else if self.wide.get() {
                    (height as f64 * 0.72) as i32
                } else {
                    (height as f64 * 0.6) as i32
                };
                frame.imp().max_height.set(max.max(120));
            }
            self.parent_size_allocate(width, height, baseline);
        }
    }

    impl BinImpl for WatchView {}
}

glib::wrapper! {
    pub struct WatchView(ObjectSubclass<imp::WatchView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

/// GtkLabel's own gestures claim every touch press on a label with links or a
/// selectable one, and claiming cancels the scrolled window's touch drag, so the
/// details couldn't be touch-scrolled from such a label. Keep touch away from the
/// label's gestures and report taps (in label coordinates) instead; mouse and pen
/// input still reach the label as usual.
fn route_label_touch(label: &gtk::Label, on_tap: impl Fn(&gtk::Label, f64, f64) + 'static) {
    use gtk::gdk::EventType;
    let start = std::rc::Rc::new(Cell::new(None::<(f64, f64)>));
    let legacy = gtk::EventControllerLegacy::new();
    legacy.set_propagation_phase(gtk::PropagationPhase::Capture);
    legacy.connect_event(move |c, ev| {
        match ev.event_type() {
            EventType::TouchBegin => start.set(ev.position()),
            EventType::TouchUpdate => {}
            EventType::TouchCancel => start.set(None),
            EventType::TouchEnd => {
                if let (Some((x0, y0)), Some((x, y))) = (start.take(), ev.position()) {
                    let label = c.widget().and_downcast::<gtk::Label>().unwrap();
                    if (x - x0).hypot(y - y0) < 10.0 {
                        // Event positions are in surface coordinates.
                        let native = label.native().unwrap();
                        let (sx, sy) = native.surface_transform();
                        let p = gtk::graphene::Point::new((x - sx) as f32, (y - sy) as f32);
                        if let Some(p) = native.compute_point(&label, &p) {
                            on_tap(&label, p.x() as f64, p.y() as f64);
                        }
                    }
                }
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    label.add_controller(legacy);
}

fn pill_button(icon: &str, label: Option<&gtk::Label>, tooltip: &str) -> gtk::Button {
    let b = gtk::Button::new();
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(icon));
    if let Some(l) = label {
        content.append(l);
    }
    b.set_child(Some(&content));
    b.add_css_class("action-pill");
    b.set_tooltip_text(Some(tooltip));
    b
}

impl WatchView {
    pub fn new(playback: &Playback) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().playback.replace(Some(playback.clone()));
        obj.build();
        obj.bind();
        obj
    }

    pub fn playback(&self) -> Playback {
        self.imp().playback.borrow().clone().unwrap()
    }

    pub fn player_view(&self) -> PlayerView {
        self.imp().player_view.borrow().clone().unwrap()
    }

    pub fn player_frame(&self) -> Option<AspectBin> {
        self.imp().player_frame.borrow().clone()
    }

    fn build(&self) {
        let imp = self.imp();
        self.add_css_class("watch-view");

        let player_view = PlayerView::new(&self.playback());
        let frame = AspectBin::new(&player_view, 16.0 / 9.0);
        frame.set_shrinkable(true);
        frame.add_css_class("player-frame");
        imp.player_view.replace(Some(player_view));
        imp.player_frame.replace(Some(frame.clone()));

        // ---- details ------------------------------------------------------
        let d = &imp.details_box;
        d.set_orientation(gtk::Orientation::Vertical);
        d.set_spacing(12);
        d.add_css_class("watch-details");

        imp.title.set_wrap(true);
        imp.title.set_xalign(0.0);
        imp.title.set_selectable(true);
        imp.title.add_css_class("watch-title");
        route_label_touch(&imp.title, |_, _, _| {});
        d.append(&imp.title);

        imp.meta.set_xalign(0.0);
        imp.meta.set_wrap(true);
        imp.meta.add_css_class("dim-label");
        imp.meta.add_css_class("caption");
        d.append(&imp.meta);

        // channel row
        let channel_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        channel_row.add_css_class("channel-row");
        imp.channel_avatar.set_size(40);
        imp.channel_avatar.set_show_initials(true);
        let names = gtk::Box::new(gtk::Orientation::Vertical, 0);
        names.set_valign(gtk::Align::Center);
        let name_row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        imp.channel_name.set_xalign(0.0);
        imp.channel_name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.channel_name.add_css_class("heading");
        imp.channel_verified.set_icon_name(Some("sloptube-verified-symbolic"));
        imp.channel_verified.add_css_class("verified");
        imp.channel_verified.set_pixel_size(12);
        name_row.append(&imp.channel_name);
        name_row.append(&imp.channel_verified);
        imp.channel_subs.set_xalign(0.0);
        imp.channel_subs.add_css_class("dim-label");
        imp.channel_subs.add_css_class("caption");
        names.append(&name_row);
        names.append(&imp.channel_subs);
        let channel_button = gtk::Button::new();
        let channel_content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        channel_content.append(&imp.channel_avatar);
        channel_content.append(&names);
        channel_button.set_child(Some(&channel_content));
        channel_button.add_css_class("flat");
        channel_button.add_css_class("channel-button");
        channel_button.set_hexpand(true);
        channel_button.set_halign(gtk::Align::Start);
        channel_button.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                if let Some(d) = view.playback().details() {
                    if !d.channel.id.is_empty() {
                        navigate(&view, Nav::Channel(d.channel.id.clone()));
                        let _ = view.activate_action("win.collapse-player", None);
                    }
                }
            }
        ));
        channel_row.append(&channel_button);
        imp.subscribe.set_label("Subscribe");
        imp.subscribe.add_css_class("subscribe-button");
        imp.subscribe.add_css_class("pill");
        imp.subscribe.set_valign(gtk::Align::Center);
        imp.subscribe.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.toggle_subscription()
        ));
        channel_row.append(&imp.subscribe);
        d.append(&channel_row);

        // actions
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.add_css_class("watch-actions");
        let like_group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        like_group.add_css_class("linked");
        like_group.add_css_class("like-group");
        let like_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        like_content.append(&gtk::Image::from_icon_name("sloptube-thumbs-up-symbolic"));
        like_content.append(&imp.like_label);
        imp.like.set_child(Some(&like_content));
        imp.like.add_css_class("action-pill");
        imp.like.set_tooltip_text(Some("I like this"));
        imp.dislike.set_icon_name("sloptube-thumbs-down-symbolic");
        imp.dislike.add_css_class("action-pill");
        imp.dislike.set_tooltip_text(Some("I dislike this"));
        imp.like.connect_toggled(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |b| view.rate(if b.is_active() { LikeStatus::Like } else { LikeStatus::Indifferent })
        ));
        imp.dislike.connect_toggled(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |b| view.rate(if b.is_active() { LikeStatus::Dislike } else { LikeStatus::Indifferent })
        ));
        like_group.append(&imp.like);
        like_group.append(&imp.dislike);
        actions.append(&like_group);

        let share = pill_button("edit-copy-symbolic", Some(&gtk::Label::new(Some("Share"))), "Copy link");
        share.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                let pb = view.playback();
                let id = pb.video_id();
                let t = pb.player().position() as u64;
                let url = if t > 5 {
                    format!("https://youtu.be/{id}?t={t}")
                } else {
                    format!("https://youtu.be/{id}")
                };
                super::copy_to_clipboard(&view, &url);
            }
        ));
        actions.append(&share);
        let download = pill_button("folder-download-symbolic", Some(&gtk::Label::new(Some("Download"))), "Download");
        download.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                if let (Some(item), Some(win)) = (view.playback().current_item(), super::main_window()) {
                    win.start_download(&item);
                }
            }
        ));
        actions.append(&download);
        let save = pill_button("sloptube-watch-later-symbolic", Some(&gtk::Label::new(Some("Save"))), "Save to Watch later");
        save.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.save_watch_later()
        ));
        actions.append(&save);
        let browser = pill_button("web-browser-symbolic", None, "Open on youtube.com");
        browser.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                let pb = view.playback();
                pb.player().pause();
                navigate(&view, Nav::Url(format!("https://www.youtube.com/watch?v={}", pb.video_id())));
            }
        ));
        actions.append(&browser);
        let actions_scroll = gtk::ScrolledWindow::builder()
            .child(&actions)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .build();
        d.append(&actions_scroll);

        // description
        imp.description_card.set_orientation(gtk::Orientation::Vertical);
        imp.description_card.add_css_class("description-card");
        imp.description.set_wrap(true);
        imp.description.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        imp.description.set_xalign(0.0);
        imp.description.set_use_markup(true);
        imp.description.connect_activate_link(|l, uri| super::activate_link(l, uri));
        route_label_touch(
            &imp.description,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |label, x, y| view.description_tapped(label, x, y)
            ),
        );
        imp.description_card.append(&imp.description);
        let click = gtk::GestureClick::new();
        click.connect_released(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, _, _, _| {
                if !view.imp().description_expanded.get() {
                    view.set_description_expanded(true);
                }
            }
        ));
        imp.description_card.add_controller(click);
        d.append(&imp.description_card);

        // comments teaser
        let cbox = gtk::Box::new(gtk::Orientation::Vertical, 4);
        imp.comments_title.set_xalign(0.0);
        imp.comments_title.add_css_class("heading");
        imp.comments_preview.set_xalign(0.0);
        imp.comments_preview.set_wrap(true);
        imp.comments_preview.set_lines(2);
        imp.comments_preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
        imp.comments_preview.add_css_class("dim-label");
        cbox.append(&imp.comments_title);
        cbox.append(&imp.comments_preview);
        imp.comments_card.set_child(Some(&cbox));
        imp.comments_card.add_css_class("comments-card");
        imp.comments_card.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.open_comments()
        ));
        d.append(&imp.comments_card);

        // queue (playlist)
        imp.queue_box.set_orientation(gtk::Orientation::Vertical);
        imp.queue_box.set_spacing(6);
        imp.queue_box.add_css_class("queue-box");
        imp.queue_title.set_xalign(0.0);
        imp.queue_title.add_css_class("heading");
        imp.queue_box.append(&imp.queue_title);
        imp.queue_list.set_orientation(gtk::Orientation::Vertical);
        imp.queue_list.set_spacing(6);
        let queue_scroll = gtk::ScrolledWindow::builder()
            .child(&imp.queue_list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(320)
            .propagate_natural_height(true)
            .build();
        imp.queue_box.append(&queue_scroll);
        imp.queue_box.set_visible(false);
        d.append(&imp.queue_box);

        imp.details_spinner.set_size_request(24, 24);
        imp.details_spinner.set_halign(gtk::Align::Center);
        d.append(&imp.details_spinner);

        // related
        imp.related_header.set_orientation(gtk::Orientation::Horizontal);
        imp.related_header.set_spacing(8);
        let up_next = gtk::Label::new(Some("Up next"));
        up_next.add_css_class("heading");
        up_next.set_hexpand(true);
        up_next.set_xalign(0.0);
        let autoplay_label = gtk::Label::new(Some("Autoplay"));
        autoplay_label.add_css_class("dim-label");
        let autoplay = gtk::Switch::new();
        autoplay.set_active(settings::get().autoplay);
        autoplay.set_valign(gtk::Align::Center);
        autoplay.connect_active_notify(|s| settings::update(|st| st.autoplay = s.is_active()));
        imp.related_header.append(&up_next);
        imp.related_header.append(&autoplay_label);
        imp.related_header.append(&autoplay);
        imp.related_header.add_css_class("related-header");
        imp.related_list.set_orientation(gtk::Orientation::Vertical);
        imp.related_list.set_spacing(20);
        imp.related_list.add_css_class("related-list");
        imp.related_more.set_label("Show more");
        imp.related_more.add_css_class("flat");
        imp.related_more.set_halign(gtk::Align::Center);
        imp.related_more.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.load_more_related()
        ));

        // Full width, left-aligned under the player (no centring clamp).
        imp.details_scroll.set_child(Some(d));
        imp.details_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        imp.details_scroll.set_vexpand(true);
        imp.details_scroll.vadjustment().connect_value_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |adj| {
                if !view.imp().wide.get() && adj.value() + adj.page_size() * 1.5 >= adj.upper() {
                    view.load_more_related();
                }
            }
        ));

        let root_page = adw::NavigationPage::builder()
            .title("Details")
            .tag("details")
            .child(&imp.details_scroll)
            .build();
        imp.nav.add(&root_page);
        imp.nav.set_vexpand(true);
        imp.nav.add_css_class("details-pane");

        imp.main_col.set_orientation(gtk::Orientation::Vertical);
        imp.main_col.set_hexpand(true);
        imp.main_col.append(&frame);
        imp.main_col.append(&imp.nav);

        imp.side_box.set_orientation(gtk::Orientation::Vertical);
        imp.side_box.set_spacing(12);
        imp.side_box.add_css_class("side-column");
        imp.side_scroll.set_child(Some(&imp.side_box));
        imp.side_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        imp.side_scroll.set_size_request(400, -1);
        // The cards' text boxes hexpand, which would otherwise propagate up through
        // the ScrolledWindow and make the box split spare width 50/50 with the player.
        imp.side_scroll.set_hexpand(false);
        imp.side_scroll.add_css_class("side-pane");
        imp.side_scroll.set_visible(false);
        imp.side_scroll.vadjustment().connect_value_changed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |adj| {
                if view.imp().wide.get() && adj.value() + adj.page_size() * 1.5 >= adj.upper() {
                    view.load_more_related();
                }
            }
        ));

        imp.columns.set_orientation(gtk::Orientation::Horizontal);
        imp.columns.append(&imp.main_col);
        imp.columns.append(&imp.side_scroll);

        let bp_bin = adw::BreakpointBin::new();
        bp_bin.set_size_request(320, 200);
        bp_bin.set_child(Some(&imp.columns));
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("min-width: 1000sp").unwrap());
        bp.connect_apply(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.set_wide(true)
        ));
        bp.connect_unapply(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.set_wide(false)
        ));
        bp_bin.add_breakpoint(bp);
        self.set_child(Some(&bp_bin));
        self.place_related();
    }

    fn bind(&self) {
        let pb = self.playback();
        pb.connect_local(
            "video-changed",
            false,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                #[upgrade_or_default]
                move |_| {
                    view.reset();
                    None
                }
            ),
        );
        pb.connect_local(
            "details-loaded",
            false,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                #[upgrade_or_default]
                move |_| {
                    if let Some(d) = view.playback().details() {
                        view.show_details(&d);
                    }
                    None
                }
            ),
        );
        pb.connect_title_notify(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |pb| {
                if !pb.title().is_empty() {
                    view.imp().title.set_text(&pb.title());
                }
            }
        ));
    }

    fn set_wide(&self, wide: bool) {
        self.imp().wide.set(wide);
        self.place_related();
    }

    pub fn set_fullscreen(&self, fullscreen: bool) {
        let imp = self.imp();
        imp.fullscreen.set(fullscreen);
        imp.nav.set_visible(!fullscreen);
        imp.side_scroll.set_visible(!fullscreen && imp.wide.get());
        if let Some(f) = imp.player_frame.borrow().as_ref() {
            f.set_vexpand(fullscreen);
        }
        self.player_view().set_fullscreen(fullscreen);
        self.queue_resize();
    }

    fn place_related(&self) {
        let imp = self.imp();
        let wide = imp.wide.get();
        for w in [
            imp.related_header.upcast_ref::<gtk::Widget>(),
            imp.related_list.upcast_ref(),
            imp.related_more.upcast_ref(),
        ] {
            if let Some(parent) = w.parent().and_downcast::<gtk::Box>() {
                parent.remove(w);
            }
        }
        let target = if wide { &imp.side_box } else { &imp.details_box };
        target.append(&imp.related_header);
        target.append(&imp.related_list);
        target.append(&imp.related_more);
        imp.side_scroll.set_visible(wide && !imp.fullscreen.get());
    }

    fn reset(&self) {
        let imp = self.imp();
        let pb = self.playback();
        imp.nav.pop_to_tag("details");
        imp.details_scroll.vadjustment().set_value(0.0);
        imp.side_scroll.vadjustment().set_value(0.0);
        imp.title.set_text(&pb.title());
        imp.meta.set_text("");
        let item = pb.current_item();
        let channel = item.as_ref().and_then(|i| i.channel_name.clone()).unwrap_or_default();
        imp.channel_name.set_text(&channel);
        imp.channel_avatar.set_text(Some(&channel));
        image::set_avatar(&imp.channel_avatar, item.as_ref().and_then(|i| i.channel_avatar.as_deref()));
        imp.channel_subs.set_text("");
        imp.channel_verified.set_visible(false);
        imp.subscribe.set_visible(false);
        imp.updating.set(true);
        imp.like.set_active(false);
        imp.dislike.set_active(false);
        imp.updating.set(false);
        imp.like_label.set_text("Like");
        imp.description.set_text("");
        imp.description_card.set_visible(false);
        imp.comments_card.set_visible(false);
        imp.queue_box.set_visible(false);
        imp.details_spinner.set_visible(true);
        imp.related_continuation.replace(None);
        imp.related_more.set_visible(false);
        while let Some(c) = imp.related_list.first_child() {
            imp.related_list.remove(&c);
        }
        imp.related_header.set_visible(false);
    }

    fn show_details(&self, d: &VideoDetails) {
        let imp = self.imp();
        imp.details_spinner.set_visible(false);
        imp.title.set_text(&d.title);
        let meta: Vec<&str> = [d.views.as_deref(), d.relative_date.as_deref().or(d.date.as_deref())]
            .into_iter()
            .flatten()
            .collect();
        imp.meta.set_text(&meta.join(" · "));

        imp.channel_name.set_text(&d.channel.name);
        imp.channel_avatar.set_text(Some(&d.channel.name));
        let scale = self.scale_factor().max(1) as u32;
        image::set_avatar(
            &imp.channel_avatar,
            d.channel
                .avatar
                .as_deref()
                .map(|u| image::sized_avatar_url(u, 40 * scale))
                .as_deref(),
        );
        imp.channel_subs.set_text(d.channel.subscribers.as_deref().unwrap_or_default());
        imp.channel_verified.set_visible(d.channel_verified);
        self.update_subscribe_button(d.subscribed);

        imp.updating.set(true);
        imp.like.set_active(d.like_status == LikeStatus::Like);
        imp.dislike.set_active(d.like_status == LikeStatus::Dislike);
        imp.updating.set(false);
        imp.like_label.set_text(d.likes.as_deref().unwrap_or("Like"));

        let has_desc = !d.description.text.trim().is_empty();
        imp.description_card.set_visible(has_desc);
        imp.description.set_markup(&super::rich_markup(&d.description));
        self.set_description_expanded(false);

        imp.comments_card.set_visible(d.comments_token.is_some());
        imp.comments_title.set_markup(&format!(
            "Comments <span alpha=\"60%\">{}</span>",
            glib::markup_escape_text(d.comment_count.as_deref().unwrap_or(""))
        ));
        imp.comments_preview.set_text("Tap to read comments");
        self.load_comment_preview(d);

        self.fill_queue();
        imp.related_continuation.replace(d.related_continuation.clone());
        self.fill_related(&d.related, true);
    }

    /// A touch tap on the description: follow the link under it, else expand.
    fn description_tapped(&self, label: &gtk::Label, x: f64, y: f64) {
        let (ox, oy) = label.layout_offsets();
        let scale = gtk::pango::SCALE as f64;
        let (inside, index, _) = label
            .layout()
            .xy_to_index(((x - ox as f64) * scale) as i32, ((y - oy as f64) * scale) as i32);
        if inside {
            // The label's text is the description's plain text, so byte indices match.
            if let Some(d) = self.playback().details() {
                let index = index as usize;
                if let Some(link) = d.description.links.iter().find(|l| l.start <= index && index < l.end) {
                    super::activate_link(label, &super::link_uri(&link.target));
                    return;
                }
            }
        }
        if !self.imp().description_expanded.get() {
            self.set_description_expanded(true);
        }
    }

    fn set_description_expanded(&self, expanded: bool) {
        let imp = self.imp();
        imp.description_expanded.set(expanded);
        if expanded {
            imp.description.set_lines(-1);
            imp.description.set_ellipsize(gtk::pango::EllipsizeMode::None);
            imp.description_card.remove_css_class("collapsed");
        } else {
            imp.description.set_lines(3);
            imp.description.set_ellipsize(gtk::pango::EllipsizeMode::End);
            imp.description_card.add_css_class("collapsed");
        }
    }

    fn update_subscribe_button(&self, subscribed: Option<bool>) {
        let imp = self.imp();
        imp.subscribe.set_visible(true);
        let subscribed = subscribed.unwrap_or(false);
        imp.subscribe.set_label(if subscribed { "Subscribed" } else { "Subscribe" });
        if subscribed {
            imp.subscribe.add_css_class("subscribed");
        } else {
            imp.subscribe.remove_css_class("subscribed");
        }
    }

    fn load_comment_preview(&self, d: &VideoDetails) {
        let Some(token) = d.comments_token.clone() else { return };
        let id = d.id.clone();
        let view = self.downgrade();
        super::spawn(async move {
            let vid = id.clone();
            let res = runtime::run(async move { api::client().comments(&token, &vid).await }).await;
            let Some(view) = view.upgrade() else { return };
            if view.playback().video_id() != id {
                return;
            }
            if let Ok(page) = res {
                if let Some(c) = page.comments.first() {
                    view.imp()
                        .comments_preview
                        .set_text(&format!("{}: {}", c.author, c.text.text.replace('\n', " ")));
                } else {
                    view.imp().comments_preview.set_text("No comments yet");
                }
                if let Some(count) = page.count {
                    view.imp().comments_title.set_markup(&format!(
                        "Comments <span alpha=\"60%\">{}</span>",
                        glib::markup_escape_text(count.trim_end_matches(" Comments"))
                    ));
                }
            }
        });
    }

    fn fill_queue(&self) {
        let imp = self.imp();
        while let Some(c) = imp.queue_list.first_child() {
            imp.queue_list.remove(&c);
        }
        let Some(q) = self.playback().queue() else {
            imp.queue_box.set_visible(false);
            return;
        };
        imp.queue_box.set_visible(true);
        imp.queue_title.set_text(&format!("{} · {}/{}", q.title, q.index + 1, q.items.len()));
        let mut current: Option<ItemCard> = None;
        for (i, v) in q.items.iter().enumerate() {
            let card = ItemCard::new(CardStyle::COMPACT);
            let mut v = v.clone();
            v.playlist_id = Some(q.playlist_id.clone());
            card.bind(&Item::Video(v));
            card.make_clickable();
            if i == q.index {
                card.add_css_class("current");
                current = Some(card.clone());
            }
            imp.queue_list.append(&card);
        }
        // Scroll the current entry into view once laid out.
        if let Some(card) = current {
            glib::idle_add_local_once(move || {
                if let Some(scroll) = card.ancestor(gtk::ScrolledWindow::static_type()).and_downcast::<gtk::ScrolledWindow>() {
                    if let Some(bounds) = card.compute_bounds(&scroll) {
                        scroll.vadjustment().set_value(bounds.y() as f64 - 8.0);
                    }
                }
            });
        }
    }

    fn fill_related(&self, items: &[Item], replace: bool) {
        let imp = self.imp();
        if replace {
            while let Some(c) = imp.related_list.first_child() {
                imp.related_list.remove(&c);
            }
        }
        // Big thumbnail cards in both layouts, like the YouTube app's tablet sidebar.
        for item in items {
            let card = ItemCard::new(CardStyle::GRID);
            card.bind(item);
            card.make_clickable();
            imp.related_list.append(&card);
        }
        imp.related_header.set_visible(imp.related_list.first_child().is_some());
        imp.related_more
            .set_visible(imp.related_continuation.borrow().is_some());
    }

    fn load_more_related(&self) {
        let imp = self.imp();
        let Some(token) = imp.related_continuation.take() else { return };
        imp.related_more.set_visible(false);
        let id = self.playback().video_id();
        let view = self.downgrade();
        super::spawn(async move {
            let res = runtime::run(async move { api::client().related_more(&token).await }).await;
            let Some(view) = view.upgrade() else { return };
            if view.playback().video_id() != id {
                return;
            }
            match res {
                Ok(page) => {
                    view.imp().related_continuation.replace(page.continuation.clone());
                    view.fill_related(&page.items, false);
                }
                Err(e) => log::warn!("more related failed: {e:#}"),
            }
        });
    }

    fn open_comments(&self) {
        let Some(d) = self.playback().details() else { return };
        let Some(token) = d.comments_token.clone() else { return };
        let page = CommentsPage::new(&d.id, &token, d.comment_count.as_deref());
        self.imp().nav.push(&page);
    }

    fn require_sign_in(&self) -> bool {
        if api::client().is_signed_in() {
            return true;
        }
        super::toast(self, "Sign in to use your YouTube account");
        false
    }

    fn rate(&self, status: LikeStatus) {
        let imp = self.imp();
        if imp.updating.get() {
            return;
        }
        if !self.require_sign_in() {
            imp.updating.set(true);
            imp.like.set_active(false);
            imp.dislike.set_active(false);
            imp.updating.set(false);
            return;
        }
        imp.updating.set(true);
        if status == LikeStatus::Like {
            imp.dislike.set_active(false);
        } else if status == LikeStatus::Dislike {
            imp.like.set_active(false);
        }
        imp.updating.set(false);
        let id = self.playback().video_id();
        let previous = self.playback().details().map(|d| d.like_status).unwrap_or_default();
        let view = self.downgrade();
        super::spawn(async move {
            let res = runtime::run(async move { api::client().rate(&id, status, previous).await }).await;
            if let (Err(e), Some(view)) = (res, view.upgrade()) {
                super::toast(&view, &format!("Couldn't rate: {e}"));
            }
        });
    }

    fn toggle_subscription(&self) {
        if !self.require_sign_in() {
            return;
        }
        let Some(d) = self.playback().details() else { return };
        let subscribed = !self.imp().subscribe.has_css_class("subscribed");
        self.update_subscribe_button(Some(subscribed));
        let channel = d.channel.id.clone();
        let view = self.downgrade();
        super::spawn(async move {
            let res = runtime::run(async move { api::client().set_subscribed(&channel, subscribed).await }).await;
            if let Some(view) = view.upgrade() {
                match res {
                    Ok(()) => super::toast(&view, if subscribed { "Subscribed" } else { "Unsubscribed" }),
                    Err(e) => {
                        view.update_subscribe_button(Some(!subscribed));
                        super::toast(&view, &format!("Couldn't update subscription: {e}"));
                    }
                }
            }
        });
    }

    fn save_watch_later(&self) {
        if !self.require_sign_in() {
            return;
        }
        let id = self.playback().video_id();
        let view = self.downgrade();
        super::spawn(async move {
            let res = runtime::run(async move { api::client().set_watch_later(&id, true).await }).await;
            if let Some(view) = view.upgrade() {
                match res {
                    Ok(()) => super::toast(&view, "Saved to Watch later"),
                    Err(e) => super::toast(&view, &format!("Couldn't save: {e}")),
                }
            }
        });
    }

    /// Back navigation inside the watch view (comments → details). Returns false at the root.
    pub fn go_back(&self) -> bool {
        let nav = &self.imp().nav;
        if nav.navigation_stack().n_items() > 1 {
            nav.pop();
            true
        } else {
            false
        }
    }
}
