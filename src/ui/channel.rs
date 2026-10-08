//! Channel page: header (banner, avatar, subscribe) and tabs (Videos, Live, Playlists, ...).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::aspect::AspectBin;
use super::card::CardStyle;
use super::feed::FeedView;
use crate::api::{self, ChannelPage as Data, FeedPage};
use crate::image;
use crate::runtime;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ChannelPage {
        pub id: RefCell<String>,
        pub data: RefCell<Option<Data>>,
        pub stack: gtk::Stack,
        pub header_revealer: gtk::Revealer,
        pub tab_stack: gtk::Stack,
        pub tabs_box: gtk::Box,
        pub subscribe: gtk::Button,
        pub status: adw::StatusPage,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChannelPage {
        const NAME: &'static str = "SlopChannelPage";
        type Type = super::ChannelPage;
        type ParentType = adw::NavigationPage;
    }

    impl ObjectImpl for ChannelPage {}
    impl WidgetImpl for ChannelPage {}
    impl NavigationPageImpl for ChannelPage {}
}

glib::wrapper! {
    pub struct ChannelPage(ObjectSubclass<imp::ChannelPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ChannelPage {
    pub fn new(id: &str) -> Self {
        let obj: Self = glib::Object::builder().property("title", "Channel").build();
        obj.imp().id.replace(id.to_owned());
        obj.build();
        obj.load();
        obj
    }

    pub fn channel_id(&self) -> String {
        self.imp()
            .data
            .borrow()
            .as_ref()
            .map(|d| d.id.clone())
            .unwrap_or_else(|| self.imp().id.borrow().clone())
    }

    fn build(&self) {
        let imp = self.imp();
        let header = adw::HeaderBar::new();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);

        let spinner = adw::Spinner::new();
        spinner.set_size_request(32, 32);
        spinner.set_halign(gtk::Align::Center);
        spinner.set_valign(gtk::Align::Center);
        imp.stack.add_named(&spinner, Some("loading"));
        let retry = gtk::Button::with_label("Retry");
        retry.add_css_class("pill");
        retry.set_halign(gtk::Align::Center);
        retry.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.load()
        ));
        imp.status.set_icon_name(Some("network-error-symbolic"));
        imp.status.set_title("Couldn't load this channel");
        imp.status.set_child(Some(&retry));
        imp.stack.add_named(&imp.status, Some("error"));
        imp.stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        tv.set_content(Some(&imp.stack));
        self.set_child(Some(&tv));
    }

    fn load(&self) {
        let imp = self.imp();
        imp.stack.set_visible_child_name("loading");
        let id = imp.id.borrow().clone();
        let page = self.downgrade();
        super::spawn(async move {
            let res = runtime::run(async move { api::client().channel(&id).await }).await;
            let Some(page) = page.upgrade() else { return };
            match res {
                Ok(d) => page.show(d),
                Err(e) => {
                    page.imp().status.set_description(Some(&glib::markup_escape_text(&e.to_string())));
                    page.imp().stack.set_visible_child_name("error");
                }
            }
        });
    }

    fn show(&self, d: Data) {
        let imp = self.imp();
        self.set_title(&d.name);
        let scale = self.scale_factor().max(1) as u32;

        // Header
        let hbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
        hbox.add_css_class("channel-header");
        if let Some(banner) = &d.banner {
            let pic = gtk::Picture::new();
            pic.set_content_fit(gtk::ContentFit::Cover);
            pic.set_can_shrink(true);
            image::set_picture(&pic, Some(banner), (1060 * scale).min(2120));
            let frame = AspectBin::new(&pic, 6.2);
            frame.add_css_class("channel-banner");
            frame.set_overflow(gtk::Overflow::Hidden);
            hbox.append(&frame);
        }
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        let avatar = adw::Avatar::new(72, Some(&d.name), true);
        image::set_avatar(
            &avatar,
            d.avatar.as_deref().map(|u| image::sized_avatar_url(u, 72 * scale)).as_deref(),
        );
        row.append(&avatar);
        let info = gtk::Box::new(gtk::Orientation::Vertical, 4);
        info.set_hexpand(true);
        info.set_valign(gtk::Align::Center);
        let name = gtk::Label::new(Some(&d.name));
        name.set_xalign(0.0);
        name.set_wrap(true);
        name.add_css_class("title-2");
        info.append(&name);
        let meta: Vec<&str> = [d.handle.as_deref(), d.subscribers.as_deref(), d.video_count.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        let meta_label = gtk::Label::new(Some(&meta.join(" · ")));
        meta_label.set_xalign(0.0);
        meta_label.set_wrap(true);
        meta_label.add_css_class("dim-label");
        info.append(&meta_label);
        if let Some(desc) = &d.description {
            let l = gtk::Label::new(Some(desc));
            l.set_xalign(0.0);
            l.set_wrap(true);
            l.set_lines(2);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            l.add_css_class("dim-label");
            l.add_css_class("caption");
            l.set_tooltip_text(Some(desc));
            info.append(&l);
        }
        row.append(&info);
        imp.subscribe.add_css_class("subscribe-button");
        imp.subscribe.add_css_class("pill");
        imp.subscribe.set_valign(gtk::Align::Center);
        self.update_subscribed(d.subscribed.unwrap_or(false));
        imp.subscribe.connect_clicked(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |_| page.toggle_subscription()
        ));
        row.append(&imp.subscribe);
        hbox.append(&row);
        imp.header_revealer.set_child(Some(&hbox));
        imp.header_revealer.set_reveal_child(true);
        imp.header_revealer.set_transition_type(gtk::RevealerTransitionType::SlideDown);

        // Tabs
        imp.tabs_box.add_css_class("channel-tabs");
        imp.tab_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        let mut first_button: Option<gtk::ToggleButton> = None;
        let initial = Rc::new(RefCell::new(Some(d.content.clone())));
        let tabs = if d.tabs.is_empty() {
            vec![api::ChannelTab {
                title: "Videos".into(),
                params: String::new(),
            }]
        } else {
            d.tabs.clone()
        };
        for (i, tab) in tabs.iter().enumerate() {
            let style = if tab.title == "Playlists" || tab.title == "Podcasts" || tab.title == "Courses" {
                CardStyle::GRID
            } else {
                CardStyle {
                    show_avatar: false,
                    show_channel: false,
                    compact: false,
                }
            };
            let feed = FeedView::new(style);
            let id = d.id.clone();
            let params = tab.params.clone();
            let init = if i == 0 { Some(initial.clone()) } else { None };
            feed.set_loader(Rc::new(move |token: Option<String>| {
                let id = id.clone();
                let params = params.clone();
                let cached: Option<FeedPage> = match (&token, &init) {
                    (None, Some(c)) => c.borrow_mut().take(),
                    _ => None,
                };
                Box::pin(async move {
                    if let Some(p) = cached {
                        return Ok(p);
                    }
                    runtime::run(async move {
                        let c = api::client();
                        match token {
                            Some(t) => c.feed_more(&t).await,
                            None => c.channel_tab(&id, &params).await,
                        }
                    })
                    .await
                })
            }));
            feed.set_empty_state("view-grid-symbolic", "This channel has nothing here");
            let reveal = imp.header_revealer.downgrade();
            feed.vadjustment().connect_value_changed(move |adj| {
                if let Some(r) = reveal.upgrade() {
                    if adj.value() > 160.0 && r.reveals_child() {
                        r.set_reveal_child(false);
                    } else if adj.value() < 4.0 && !r.reveals_child() {
                        r.set_reveal_child(true);
                    }
                }
            });
            imp.tab_stack.add_named(&feed, Some(&tab.title));

            let b = gtk::ToggleButton::with_label(&tab.title);
            b.add_css_class("flat");
            b.add_css_class("tab-button");
            if let Some(f) = &first_button {
                b.set_group(Some(f));
            } else {
                first_button = Some(b.clone());
                b.set_active(true);
            }
            let title = tab.title.clone();
            b.connect_toggled(glib::clone!(
                #[weak(rename_to = page)]
                self,
                #[weak]
                feed,
                move |b| {
                    if b.is_active() {
                        page.imp().tab_stack.set_visible_child_name(&title);
                        feed.ensure_loaded();
                    }
                }
            ));
            imp.tabs_box.append(&b);
            if i == 0 {
                feed.ensure_loaded();
            }
        }
        let tabs_scroll = gtk::ScrolledWindow::builder()
            .child(&imp.tabs_box)
            .hscrollbar_policy(gtk::PolicyType::External)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.append(&imp.header_revealer);
        content.append(&tabs_scroll);
        content.append(&imp.tab_stack);
        imp.stack.add_named(&content, Some("content"));
        imp.stack.set_visible_child_name("content");
        imp.data.replace(Some(d));
    }

    fn update_subscribed(&self, subscribed: bool) {
        let b = &self.imp().subscribe;
        b.set_label(if subscribed { "Subscribed" } else { "Subscribe" });
        if subscribed {
            b.add_css_class("subscribed");
        } else {
            b.remove_css_class("subscribed");
        }
    }

    fn toggle_subscription(&self) {
        if !api::client().is_signed_in() {
            super::toast(self, "Sign in to subscribe");
            return;
        }
        let subscribed = !self.imp().subscribe.has_css_class("subscribed");
        self.update_subscribed(subscribed);
        let id = self.channel_id();
        let page = self.downgrade();
        super::spawn(async move {
            let res = runtime::run(async move { api::client().set_subscribed(&id, subscribed).await }).await;
            if let (Err(e), Some(page)) = (res, page.upgrade()) {
                page.update_subscribed(!subscribed);
                super::toast(&page, &format!("Couldn't update subscription: {e}"));
            }
        });
    }
}
