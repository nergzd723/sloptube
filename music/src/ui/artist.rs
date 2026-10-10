//! Artist (and user profile) pages.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::art::ArtView;
use super::browse::shelves_to_rows;
use super::rows::{Loader, Row, RowList};
use super::actions;
use crate::api::{self, ArtistPage, BrowseTarget, Item, ShelfKind};
use crate::library::Library;
use crate::playback::Playback;
use crate::runtime;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ArtistView {
        pub target: RefCell<Option<BrowseTarget>>,
        pub page: RefCell<Option<ArtistPage>>,
        pub rows: RowList,
        pub header: gtk::Overlay,
        pub banner: RefCell<Option<ArtView>>,
        pub name: gtk::Label,
        pub audience: gtk::Label,
        pub shuffle: gtk::Button,
        pub radio: gtk::Button,
        pub save: gtk::Button,
        pub on_title: RefCell<Option<Box<dyn Fn(&str)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArtistView {
        const NAME: &'static str = "StmArtistView";
        type Type = super::ArtistView;
        type ParentType = adw::Bin;
    }

    impl ObjectImpl for ArtistView {}
    impl WidgetImpl for ArtistView {}
    impl BinImpl for ArtistView {}
}

glib::wrapper! {
    pub struct ArtistView(ObjectSubclass<imp::ArtistView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ArtistView {
    pub fn new(target: BrowseTarget) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().target.replace(Some(target));
        obj.build();
        obj.imp().rows.set_loader(obj.loader());
        obj.imp().rows.reload();
        obj
    }

    pub fn connect_title(&self, f: impl Fn(&str) + 'static) {
        self.imp().on_title.replace(Some(Box::new(f)));
    }

    fn build(&self) {
        let imp = self.imp();
        let banner = ArtView::new_banner(300);
        banner.add_css_class("artist-banner");
        imp.header.set_child(Some(&banner));
        let fade = gtk::Box::new(gtk::Orientation::Vertical, 0);
        fade.add_css_class("banner-fade");
        fade.set_can_target(false);
        imp.header.add_overlay(&fade);

        let info = gtk::Box::new(gtk::Orientation::Vertical, 8);
        info.add_css_class("artist-info");
        info.set_valign(gtk::Align::End);
        imp.name.set_xalign(0.0);
        imp.name.set_wrap(true);
        imp.name.add_css_class("artist-name");
        imp.audience.set_xalign(0.0);
        imp.audience.add_css_class("dimmed");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let pill = |b: &gtk::Button, icon: &str, label: &str| {
            let content = adw::ButtonContent::builder().icon_name(icon).label(label).build();
            b.set_child(Some(&content));
            b.add_css_class("pill-small");
        };
        pill(&imp.shuffle, "media-playlist-shuffle-symbolic", "Shuffle");
        imp.shuffle.add_css_class("primary-pill");
        pill(&imp.radio, "stm-radio-symbolic", "Mix");
        pill(&imp.save, "stm-save-symbolic", "Save");
        imp.shuffle.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                let page = view.imp().page.borrow().clone();
                if let Some(p) = page
                    && let Some(t) = p.shuffle.clone() {
                        Playback::get().play_watch(t, &p.name);
                    }
            }
        ));
        imp.radio.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                let page = view.imp().page.borrow().clone();
                if let Some(p) = page
                    && let Some(t) = p.radio.clone() {
                        Playback::get().play_watch(t, &format!("{} radio", p.name));
                    }
            }
        ));
        imp.save.connect_clicked(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                let card = view.imp().page.borrow().as_ref().map(ArtistPage::card);
                if let Some(c) = card {
                    let lib = Library::get();
                    let saved = !lib.is_saved(&c.target.id);
                    lib.set_saved(&c, saved);
                    super::toast(if saved { "Saved to library" } else { "Removed from library" });
                    view.update_saved();
                }
            }
        ));
        let more = gtk::MenuButton::new();
        more.set_icon_name("view-more-symbolic");
        more.add_css_class("circular");
        more.add_css_class("header-action");
        more.set_create_popup_func(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |mb| {
                let card = view.imp().page.borrow().as_ref().map(ArtistPage::card);
                if let Some(c) = card {
                    mb.set_menu_model(Some(&actions::menu_for(&Item::Card(c))));
                }
            }
        ));
        let get: Rc<dyn Fn() -> Option<Item>> = {
            let weak = self.downgrade();
            Rc::new(move || {
                weak.upgrade()
                    .and_then(|v| v.imp().page.borrow().as_ref().map(ArtistPage::card))
                    .map(Item::Card)
            })
        };
        actions::install(&more, get);
        buttons.append(&imp.shuffle);
        buttons.append(&imp.radio);
        buttons.append(&imp.save);
        buttons.append(&more);
        info.append(&imp.name);
        info.append(&imp.audience);
        info.append(&buttons);
        imp.header.add_overlay(&info);
        imp.header.add_css_class("artist-header");
        imp.header.set_visible(false);
        imp.banner.replace(Some(banner));

        imp.rows.set_prefix(vec![Row::Widget(imp.header.clone().upcast())]);
        self.set_child(Some(&imp.rows));
        super::on_layout(self, glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |narrow| {
                if let Some(b) = view.imp().banner.borrow().as_ref() {
                    b.set_banner_height(if narrow { 300 } else { 380 });
                }
            }
        ));
    }

    fn update_saved(&self) {
        let imp = self.imp();
        let saved = imp
            .page
            .borrow()
            .as_ref()
            .is_some_and(|p| Library::get().is_saved(&p.id));
        let content = adw::ButtonContent::builder()
            .icon_name(if saved { "stm-saved-symbolic" } else { "stm-save-symbolic" })
            .label(if saved { "Saved" } else { "Save" })
            .build();
        imp.save.set_child(Some(&content));
    }

    fn fill(&self, page: &ArtistPage) {
        let imp = self.imp();
        imp.name.set_text(&page.name);
        let audience = page
            .audience
            .clone()
            .or_else(|| page.subscribers.as_ref().map(|s| format!("{s} subscribers")))
            .unwrap_or_default();
        imp.audience.set_text(&audience);
        imp.audience.set_visible(!audience.is_empty());
        imp.shuffle.set_visible(page.shuffle.is_some());
        imp.radio.set_visible(page.radio.is_some());
        if let Some(b) = imp.banner.borrow().as_ref() {
            b.set_thumbnails(&page.thumbnails);
            b.set_visible(!page.thumbnails.is_empty());
        }
        imp.header.set_visible(true);
        if let Some(f) = imp.on_title.borrow().as_ref() {
            f(&page.name);
        }
    }

    fn loader(&self) -> Loader {
        let weak = self.downgrade();
        Rc::new(move |_| {
            let weak = weak.clone();
            Box::pin(async move {
                let Some(view) = weak.upgrade() else {
                    return Ok((Vec::new(), None));
                };
                let id = view.imp().target.borrow().as_ref().map(|t| t.id.clone()).unwrap_or_default();
                drop(view);
                let page = runtime::run(async move { api::client().artist(&id).await }).await?;
                let Some(view) = weak.upgrade() else {
                    return Ok((Vec::new(), None));
                };
                view.fill(&page);
                // The description goes last, like on YouTube Music.
                let (text, other): (Vec<_>, Vec<_>) = page.shelves.iter().cloned().partition(|s| s.kind == ShelfKind::Text);
                let mut rows = shelves_to_rows(other);
                rows.extend(shelves_to_rows(text));
                view.imp().page.replace(Some(page));
                view.update_saved();
                Ok((rows, None))
            })
        })
    }
}

pub fn artist_page(target: &BrowseTarget, title: &str) -> adw::NavigationPage {
    let view = ArtistView::new(target.clone());
    let chrome = super::page::page(title, None, &view, false);
    chrome.toolbar.set_extend_content_to_top_edge(true);
    chrome.header.add_css_class("over-banner");
    let page = chrome.page.clone();
    view.connect_title(move |t| page.set_title(t));
    chrome.page
}
