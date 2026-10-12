//! Comments and replies (in the watch view's side panel or details navigation).

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::{Nav, navigate};
use crate::api::{self, Comment, CommentsPage as Page};
use crate::image;
use crate::runtime;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct CommentsPage {
        pub video_id: RefCell<String>,
        pub list: gtk::ListBox,
        pub scroll: gtk::ScrolledWindow,
        pub continuation: RefCell<Option<String>>,
        pub loading: Cell<bool>,
        pub spinner: adw::Spinner,
        pub sort: gtk::DropDown,
        pub sort_tokens: RefCell<Vec<String>>,
        pub generation: Cell<u64>,
        pub header_comment: RefCell<Option<Comment>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CommentsPage {
        const NAME: &'static str = "SlopCommentsPage";
        type Type = super::CommentsPage;
        type ParentType = adw::NavigationPage;
    }

    impl ObjectImpl for CommentsPage {}
    impl WidgetImpl for CommentsPage {}
    impl NavigationPageImpl for CommentsPage {}
}

glib::wrapper! {
    pub struct CommentsPage(ObjectSubclass<imp::CommentsPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CommentsPage {
    pub fn new(video_id: &str, token: &str, count: Option<&str>) -> Self {
        let title = match count {
            Some(c) => format!("Comments · {c}"),
            None => "Comments".to_string(),
        };
        let obj: Self = glib::Object::builder().property("title", &title).build();
        obj.imp().video_id.replace(video_id.to_owned());
        obj.build(true);
        obj.load(Some(token.to_owned()), true);
        obj
    }

    /// A page with the replies of one comment.
    pub fn replies(video_id: &str, parent: &Comment, token: &str) -> Self {
        let obj: Self = glib::Object::builder().property("title", "Replies").build();
        obj.imp().video_id.replace(video_id.to_owned());
        obj.imp().header_comment.replace(Some(parent.clone()));
        obj.build(false);
        obj.load(Some(token.to_owned()), true);
        obj
    }

    fn build(&self, with_sort: bool) {
        let imp = self.imp();
        let header = adw::HeaderBar::new();
        header.set_show_end_title_buttons(false);
        header.set_show_start_title_buttons(false);
        if with_sort {
            // The top-level page can be closed wherever it is shown (see `WatchView`).
            let close = gtk::Button::from_icon_name("window-close-symbolic");
            close.set_tooltip_text(Some("Close comments"));
            close.set_action_name(Some("watch.close-comments"));
            header.pack_end(&close);
            imp.sort.set_model(Some(&gtk::StringList::new(&["Top", "Newest"])));
            imp.sort.set_tooltip_text(Some("Sort comments"));
            imp.sort.set_sensitive(false);
            imp.sort.connect_selected_notify(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |dd| {
                    let token = page.imp().sort_tokens.borrow().get(dd.selected() as usize).cloned();
                    if let Some(t) = token {
                        page.load(Some(t), true);
                    }
                }
            ));
            header.pack_end(&imp.sort);
        }
        imp.list.set_selection_mode(gtk::SelectionMode::None);
        imp.list.add_css_class("comments-list");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        if let Some(c) = imp.header_comment.borrow().as_ref() {
            let parent = self.comment_widget(c, false);
            parent.add_css_class("parent-comment");
            content.append(&parent);
            content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        content.append(&imp.list);
        imp.spinner.set_size_request(24, 24);
        imp.spinner.set_margin_top(12);
        imp.spinner.set_margin_bottom(12);
        content.append(&imp.spinner);
        let clamp = adw::Clamp::builder().maximum_size(900).child(&content).build();
        imp.scroll.set_child(Some(&clamp));
        super::track_scrolling(&imp.scroll);
        imp.scroll.set_vexpand(true);
        imp.scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        imp.scroll.vadjustment().connect_value_changed(glib::clone!(
            #[weak(rename_to = page)]
            self,
            move |adj| {
                if adj.value() + adj.page_size() * 2.0 >= adj.upper() {
                    let token = page.imp().continuation.borrow().clone();
                    if token.is_some() {
                        page.load(token, false);
                    }
                }
            }
        ));
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&header);
        tv.set_content(Some(&imp.scroll));
        self.set_child(Some(&tv));
    }

    fn load(&self, token: Option<String>, replace: bool) {
        let imp = self.imp();
        if imp.loading.get() && !replace {
            return;
        }
        let Some(token) = token else { return };
        let ticket = imp.generation.get() + 1;
        imp.generation.set(ticket);
        imp.loading.set(true);
        imp.spinner.set_visible(true);
        imp.continuation.replace(None);
        let vid = imp.video_id.borrow().clone();
        let page = self.downgrade();
        super::spawn(async move {
            let res = runtime::run(async move { api::client().comments(&token, &vid).await }).await;
            let Some(page) = page.upgrade() else { return };
            let imp = page.imp();
            if imp.generation.get() != ticket {
                return;
            }
            imp.loading.set(false);
            imp.spinner.set_visible(false);
            match res {
                Ok(p) => page.apply(p, replace),
                Err(e) => super::toast(&page, &format!("Couldn't load comments: {e}")),
            }
        });
    }

    fn apply(&self, p: Page, replace: bool) {
        let imp = self.imp();
        if replace {
            imp.list.remove_all();
        }
        if !p.sort_options.is_empty() {
            imp.sort_tokens
                .replace(p.sort_options.iter().map(|(_, t, _)| t.clone()).collect());
            imp.sort.set_sensitive(true);
        }
        for c in &p.comments {
            let row = gtk::ListBoxRow::new();
            row.set_activatable(false);
            row.set_child(Some(&self.comment_widget(c, true)));
            imp.list.append(&row);
        }
        if p.comments.is_empty() && imp.list.first_child().is_none() {
            let l = gtk::Label::new(Some("No comments"));
            l.add_css_class("dim-label");
            l.set_margin_top(24);
            imp.list.append(&l);
        }
        imp.continuation.replace(p.continuation);
    }

    fn comment_widget(&self, c: &Comment, show_replies: bool) -> gtk::Widget {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("comment");
        let avatar = adw::Avatar::new(32, Some(&c.author), true);
        avatar.set_valign(gtk::Align::Start);
        let scale = self.scale_factor().max(1) as u32;
        image::set_avatar(
            &avatar,
            c.author_avatar
                .as_deref()
                .map(|u| image::sized_avatar_url(u, 32 * scale))
                .as_deref(),
        );
        if let Some(id) = c.author_channel_id.clone() {
            let click = gtk::GestureClick::new();
            click.connect_released(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_, _, _, _| {
                    if super::press_stopped_scrolling(&page.imp().scroll) {
                        return;
                    }
                    navigate(&page, Nav::Channel(id.clone()));
                    let _ = page.activate_action("win.collapse-player", None);
                }
            ));
            avatar.add_controller(click);
            avatar.set_cursor_from_name(Some("pointer"));
        }
        row.append(&avatar);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 4);
        body.set_hexpand(true);
        let head = gtk::Label::new(None);
        head.set_xalign(0.0);
        head.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let mut markup = format!(
            "<b>{}</b>",
            glib::markup_escape_text(&c.author)
        );
        if c.is_creator {
            markup = format!("<span background=\"#80808040\"> {markup} </span>");
        }
        markup.push_str(&format!(
            " <span alpha=\"60%\">{}</span>",
            glib::markup_escape_text(&c.published)
        ));
        if c.pinned {
            markup = format!("<span alpha=\"60%\">📌 Pinned</span>  {markup}");
        }
        head.set_markup(&markup);
        head.add_css_class("caption");
        body.append(&head);
        let text = super::rich_label(&c.text);
        body.append(&text);

        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let likes = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        likes.append(&gtk::Image::from_icon_name("sloptube-thumbs-up-symbolic"));
        likes.append(&gtk::Label::new(c.likes.as_deref()));
        likes.add_css_class("dim-label");
        likes.add_css_class("caption");
        footer.append(&likes);
        if c.hearted {
            let heart = gtk::Label::new(Some("♥"));
            heart.add_css_class("heart");
            heart.set_tooltip_text(Some("Hearted by the creator"));
            footer.append(&heart);
        }
        body.append(&footer);
        if show_replies {
            if let (Some(count), Some(token)) = (c.reply_count.clone(), c.replies_token.clone()) {
                let b = gtk::Button::with_label(&format!(
                    "{count} {}",
                    if count == "1" { "reply" } else { "replies" }
                ));
                b.add_css_class("flat");
                b.add_css_class("replies-button");
                b.set_halign(gtk::Align::Start);
                let parent = c.clone();
                b.connect_clicked(glib::clone!(
                    #[weak(rename_to = page)]
                    self,
                    move |_| {
                        let vid = page.imp().video_id.borrow().clone();
                        let replies = CommentsPage::replies(&vid, &parent, &token);
                        if let Some(nav) = page
                            .ancestor(adw::NavigationView::static_type())
                            .and_downcast::<adw::NavigationView>()
                        {
                            nav.push(&replies);
                        }
                    }
                ));
                body.append(&b);
            }
        }
        row.append(&body);
        row.upcast()
    }
}
