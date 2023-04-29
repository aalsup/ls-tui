use std::{fmt, fs, io, thread, time};
use std::cmp::Ordering;
use std::fs::{DirEntry, FileType, Permissions};
#[cfg(target_os = "linux")]
use std::os::linux::fs::MetadataExt;
#[cfg(target_os = "macos")]
use std::os::macos::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::time::{Instant, SystemTime};

use byte_unit::Byte;
use fs_extra::dir::get_size;
use itertools::Itertools;
use notify::{Watcher};
use tui::style::{Modifier, Style};
use tui::widgets::{ListState, Row, TableState};
use unix_permissions_ext::UNIXPermissionsExt;
use users::get_user_by_uid;
use tracing;

use crate::AppError;

#[derive(Debug, Clone)]
pub enum SortByDirection {
    Asc,
    Dec,
}

impl Default for SortByDirection {
    fn default() -> Self {
        SortByDirection::Asc
    }
}

#[derive(Debug, Clone)]
pub enum SortBy {
    TypeName(SortByDirection),
    Name(SortByDirection),
    DateTime(SortByDirection),
    Size(SortByDirection),
}

impl SortBy {
    pub(crate) fn all() -> Vec<SortBy> {
        vec![
            SortBy::TypeName(SortByDirection::Asc),
            SortBy::TypeName(SortByDirection::Dec),
            SortBy::DateTime(SortByDirection::Asc),
            SortBy::DateTime(SortByDirection::Dec),
            SortBy::Name(SortByDirection::Asc),
            SortBy::Name(SortByDirection::Dec),
            SortBy::Size(SortByDirection::Asc),
            SortBy::Size(SortByDirection::Dec),
        ]
    }
}

impl fmt::Display for SortBy {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let output: String = match self {
            SortBy::TypeName(SortByDirection::Asc) => "TypeName (ASC)".to_string(),
            SortBy::TypeName(SortByDirection::Dec) => "TypeName (DEC)".to_string(),
            SortBy::DateTime(SortByDirection::Asc) => "DateTime (ASC)".to_string(),
            SortBy::DateTime(SortByDirection::Dec) => "DateTime (DEC)".to_string(),
            SortBy::Name(SortByDirection::Asc) => "Name (ASC)".to_string(),
            SortBy::Name(SortByDirection::Dec) => "Name (DEC)".to_string(),
            SortBy::Size(SortByDirection::Asc) => "Size (ASC)".to_string(),
            SortBy::Size(SortByDirection::Dec) => "Size (DEC)".to_string(),
        };
        write!(f, "{:?}", output)
    }
}

#[derive(Debug, Clone)]
pub struct DirEntryData {
    pub name: String,
    pub file_type: FileType,
    pub size: Option<u64>,
    pub uid: u32,
    pub gid: u32,
    pub permissions: Permissions,
    pub modified: SystemTime,
}

#[derive(Debug)]
pub struct SizeNotification {
    pub name: String,
    pub size: u64,
}

impl From<&PathBuf> for DirEntryData {
    fn from(path: &PathBuf) -> Self {
        let file_name = path.file_name()
            .expect("unable to read file_name")
            .to_str()
            .expect("unable to get file_name as str")
            .to_string();
        let meta = path.metadata()
            .expect(format!("unable to get metadata for {}", file_name).as_str());
        let file_type = meta.file_type();
        let mut file_size: Option<u64> = None;
        if file_type.is_file() {
            file_size = Some(meta.len());
        }
        let uid = meta.st_uid();
        let gid = meta.st_gid();
        let permissions = meta.permissions();
        let modified = meta.modified()
            .expect("Unable to get modified from DirEntry");
        DirEntryData {
            name: file_name,
            file_type,
            size: file_size,
            uid,
            gid,
            permissions,
            modified,
        }
    }
}

impl From<DirEntry> for DirEntryData {
    fn from(dir_entry: DirEntry) -> Self {
        let file_name = dir_entry.file_name().into_string()
            .expect("Unable to get filename from DirEntry");
        let file_type = dir_entry.file_type()
            .expect("Unable to get file_type from DirEntry");
        let meta = dir_entry.metadata()
            .expect("Unable to get metadata from DirEntry");
        let mut file_size: Option<u64> = None;
        if file_type.is_file() {
            // only get file sizes now; otherwise, async via `register_size_watcher()`
            file_size = Some(meta.len());
        }
        let uid = meta.st_uid();
        let gid = meta.st_gid();
        let permissions = meta.permissions();
        let modified = meta.modified()
            .expect("Unable to get modified from DirEntry");
        DirEntryData {
            name: file_name,
            file_type,
            size: file_size,
            uid,
            gid,
            permissions,
            modified,
        }
    }
}

impl From<DirectoryListItem> for Row<'_> {
    fn from(item: DirectoryListItem) -> Self {
        let style = Style::default();
        let dir_style = style.add_modifier(Modifier::BOLD);
        let link_style = style.add_modifier(Modifier::ITALIC);

        match item {
            DirectoryListItem::ParentDir(item) => {
                let file_name = item;
                //Row::new(vec![file_name.as_str()]).style(dir_style)
                Row::new(vec![file_name]).style(dir_style)
            }
            DirectoryListItem::Entry(item) => {
                let file_name = item.name.clone();
                // determine the type of file (directory, symlink, etc.)
                let mut style = Style::default();
                if item.file_type.is_dir() {
                    style = dir_style;
                }
                if item.file_type.is_symlink() {
                    style = link_style;
                }
                let filesize_str = {
                    if let Some(size) = item.size {
                        let byte = Byte::from_bytes(size.into());
                        let adjusted_byte = byte.get_appropriate_unit(false);
                        adjusted_byte.to_string()
                    } else {
                        "...".to_string()
                    }
                };
                let mut user = item.uid.to_string();
                if let Some(uname) = get_user_by_uid(item.uid) {
                    user = uname.name().to_os_string().into_string()
                        .expect("unable to convert username to string");
                }
                let gid = item.gid.to_string();
                let perms = item.permissions.clone();
                let perms_str = perms.stringify();
                let user_perms = perms_str[0..3].to_string();
                let group_perms = perms_str[3..6].to_string();
                let other_perms = perms_str[6..9].to_string();

                Row::new(vec![
                    file_name,
                    filesize_str,
                    user,
                    gid,
                    user_perms,
                    group_perms,
                    other_perms,
                ])
                    .style(style)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum DirectoryListItem {
    Entry(DirEntryData),
    ParentDir(String),
}

/// This struct holds the current state of the app. In particular, it has the `items` field which is a wrapper
/// around `ListState`. Keeping track of the items state let us render the associated widget with its state
/// and have access to features such as natural scrolling.
#[derive(Debug)]
pub struct DirectoryList {
    pub dir: String,
    pub sort_by: SortBy,
    pub sort_by_list_state: ListState,
    pub state: TableState,
    pub items: Vec<DirectoryListItem>,
    // changing directory
    pub dir_change_tx: Option<Sender<String>>,
    // watched directory
    pub dir_watch_tx: Option<Sender<notify::Event>>,
    pub dir_watch_rx: Option<Receiver<notify::Event>>,
    // dir size computed
    pub dir_size_tx: Option<Sender<SizeNotification>>,
    pub dir_size_rx: Option<Receiver<SizeNotification>>,
    // event tx
    pub event_tx: Sender<String>,
}

impl DirectoryList {
    pub(crate) fn new(dir_name: String, event_tx: Sender<String>) -> Self {
        Self {
            dir: dir_name,
            state: TableState::default(),
            items: vec![],
            dir_change_tx: None,
            dir_watch_tx: None,
            sort_by: SortBy::TypeName(SortByDirection::Asc),
            sort_by_list_state: {
                let mut state = ListState::default();
                state.select(Some(0));
                state
            },
            dir_size_rx: None,
            dir_size_tx: None,
            event_tx,
            dir_watch_rx: None,
        }
    }

    #[tracing::instrument]
    pub(crate) fn watch(&mut self) -> Result<(), AppError> {
        match &self.dir_change_tx {
            Some(dir_change_tx) => {
                // send the new directory to the watcher thread
                let new_dir = self.dir.clone();
                let dir_change_tx = dir_change_tx.clone();
                dir_change_tx.send(new_dir)
                    .expect("unable to send dir_change event");
            }
            None => {
                let dir = self.dir.clone();
                let (dir_change_tx, dir_change_rx): (Sender<String>, Receiver<String>) = channel();
                self.dir_change_tx = Some(dir_change_tx.clone());
                // this is used to receive updates from the watcher
                let (dir_watch_tx, dir_watch_rx): (Sender<notify::Event>, Receiver<notify::Event>) = channel();
                self.dir_watch_tx = Some(dir_watch_tx.clone());
                self.dir_watch_rx = Some(dir_watch_rx);
                thread::spawn(move || {
                    let mut watcher = notify::recommended_watcher(move |res| {
                        match res {
                            Ok(event) => {
                                let _result = dir_watch_tx.send(event);
                            }
                            Err(_) => {}
                        }
                    }).expect("unable to create recommended_watcher");
                    watcher.watch(Path::new(dir.as_str()), notify::RecursiveMode::NonRecursive)
                        .expect("unable to watch dir");
                    let mut cur_dir = dir.clone();
                    loop {
                        let dir_event = dir_change_rx.try_recv();
                        match dir_event {
                            Ok(new_dir) => {
                                tracing::debug!("dir watcher thread received a new dir: {}", new_dir);
                                // changed directory to watch
                                watcher.unwatch(Path::new(cur_dir.as_str()))
                                    .expect("unable to unwatch dir");
                                cur_dir = new_dir.clone();
                                watcher.watch(Path::new(new_dir.as_str()), notify::RecursiveMode::NonRecursive)
                                    .expect("unable to watch new dir");
                            }
                            Err(TryRecvError::Empty) => {
                                // nothing in the channel
                            }
                            Err(TryRecvError::Disconnected) => {
                                break;
                            }
                        }
                        let sleep_millis = time::Duration::from_millis(250);
                        thread::sleep(sleep_millis);
                    }
                });
            }
        }
        Ok(())
    }

    #[tracing::instrument]
    fn register_size_calculator(&mut self, data: DirEntryData) {
        if self.dir_size_rx.is_none() {
            // construct a new channel
            let (tx, rx): (Sender<SizeNotification>, Receiver<SizeNotification>) = channel();
            self.dir_size_tx = Some(tx);
            self.dir_size_rx = Some(rx);
        }

        let parent_dir = self.dir.clone();
        let event_tx = self.event_tx.clone();
        if let Some(dir_size_tx) = &self.dir_size_tx {
            let dir_size_tx = dir_size_tx.clone();
            // execute the expensive `get_size()` in a separate thread (not within the tokio executor)
            thread::spawn(move || {
                let cur_path = Path::new(parent_dir.as_str());
                let file_path = cur_path.join(&data.name).canonicalize()
                    .expect("unable to canonicalize path for getting dir_size");
                tracing::debug!("Computing dir size for {}", data.name.clone());
                let start = Instant::now();
                let dir_size = get_size(file_path).unwrap_or(0);
                let duration = start.elapsed();
                dir_size_tx.send(
                    SizeNotification {
                        name: data.name.clone(),
                        size: dir_size,
                    }
                ).expect("unable to send dir_size_tx from thread");
                tracing::debug!("Dir size for {} in {:?}", data.name.clone(), duration);
                event_tx.send(format!("Dir size for {} in {:?}", data.name.clone(), duration))
                    .expect("unable to send event_tx for directory size");
            });
        }
    }

    #[tracing::instrument]
    pub(crate) fn smart_refresh(&mut self, fs_events: Vec<notify::Event>) -> Result<(), io::Error> {
        // Bug: `rm file1` generates both Create(File) and Remove(File) events.
        tracing::info!("smart_refresh() called with {} events", fs_events.len());

        let mut create_files: Vec<&PathBuf> = vec![];
        let mut modify_files: Vec<&PathBuf> = vec![];
        let mut remove_files: Vec<&PathBuf> = vec![];

        // collect filenames into proper vecs
        for fs_event in &fs_events {
            match fs_event.kind {
                notify::EventKind::Create(_) => {
                    for path in &fs_event.paths {
                        create_files.push(path);
                    }
                },
                notify::EventKind::Modify(_) => {
                    for path in &fs_event.paths {
                        modify_files.push(path);
                    }
                },
                notify::EventKind::Remove(_) => {
                    for path in &fs_event.paths {
                        remove_files.push(path);
                    }
                },
                _ => {}
            }
        }

        // filter out create events that also have remove events
        let start_count = create_files.len();
        let create_files: Vec<&PathBuf> = create_files
            .into_iter()
            .filter(|create_path| {
                !(*remove_files).contains(&create_path)
            })
            .unique()
            .collect();
        tracing::debug!("create_files: initial={}, filtered={}", start_count, create_files.len());

        // filter out modify events that also have remove events
        let start_count = modify_files.len();
        let modify_files: Vec<&PathBuf> = modify_files
            .into_iter()
            .filter(|modify_path| {
                !(*remove_files).contains(&modify_path)
            })
            .unique()
            .collect();
        tracing::debug!("modify_files: initial={}, filtered={}", start_count, modify_files.len());

        // filter out duplicates
        let start_count = remove_files.len();
        let remove_files: Vec<&PathBuf> = remove_files
            .into_iter()
            .unique()
            .collect();
        tracing::debug!("remove_files: initial={}, filtered={}", start_count, remove_files.len());

        // process removed files
        for remove_path in remove_files {
            let file_name = remove_path.file_name()
                .expect("unable to extract file_name")
                .to_str()
                .expect("unable to convert file_name to str")
                .to_string();
            self.items.retain(|x| {
                match x {
                    DirectoryListItem::ParentDir(_) => true,
                    DirectoryListItem::Entry(e) => {
                        if e.name == file_name {
                            tracing::info!("removing file {}", file_name);
                            false
                        } else {
                            true
                        }
                    }
                }
            });
        }

        // process create files
        for create_path in create_files {
            let data: DirEntryData = create_path.into();
            if data.file_type.is_dir() || data.file_type.is_symlink() {
                self.register_size_calculator(data.clone());
            }
            let filename = data.name.clone();
            tracing::debug!("Adding file {}", filename);
            let data_item = DirectoryListItem::Entry(data);
            self.items.push(data_item);
        }

        // process modified files
        let mut heavy_refresh_needed = false;
        for fs_event in fs_events {
            match &fs_event.kind {
                notify::EventKind::Modify(modify_kind) => {
                    match modify_kind {
                        notify::event::ModifyKind::Name(_name_change) => {
                            // notify breaks name changes into 2 separate events
                            tracing::debug!("name changed: requires heavy refresh");
                            heavy_refresh_needed = true;
                            break;
                        },
                        _ => {
                            // file permissions (etc.) may have changed
                            for path in &fs_event.paths {
                                let file_name = path.file_name()
                                    .expect("unable to extract file_name")
                                    .to_str()
                                    .expect("unable to convert file_name to str")
                                    .to_string();
                                tracing::debug!("changing file {}", file_name);
                                // remove this item from the list
                                self.items.retain(|item| {
                                    match item {
                                        DirectoryListItem::ParentDir(_) => true,
                                        DirectoryListItem::Entry(item) => {
                                            if item.name == file_name {
                                                false
                                            } else {
                                                true
                                            }
                                        }
                                    }
                                });
                                // refresh the modified file
                                let data: DirEntryData = path.into();
                                if data.file_type.is_dir() || data.file_type.is_symlink() {
                                    self.register_size_calculator(data.clone());
                                }
                                let filename = data.name.clone();
                                tracing::debug!("Refreshing file {}", filename);
                                let data_item = DirectoryListItem::Entry(data);
                                self.items.push(data_item);
                            }
                        }
                    }
                },
                _ => {}
            }
        }

        if heavy_refresh_needed {
            tracing::debug!("call standard refresh() due to events seen");
            self.refresh().expect("refresh() errored");
        } else {
            self.items
                .sort_by(|a, b| DirectoryList::compare_dir_items(a, b, &self.sort_by));
        }

        Ok(())
    }

    #[tracing::instrument]
    pub(crate) fn refresh(&mut self) -> Result<(), io::Error> {
        self.items.clear();
        // read all the items in the directory
        self.items = fs::read_dir(self.dir.clone())?
            .into_iter()
            .map(|x| x.expect("unable to get DirEntry from iterator"))
            .map(|x| {
                let data: DirEntryData = x.into();
                if data.file_type.is_dir() || data.file_type.is_symlink() {
                    self.register_size_calculator(data.clone());
                }
                data
            })
            .map(|x| DirectoryListItem::Entry(x))
            .collect();
        self.items
            .insert(0, DirectoryListItem::ParentDir("..".to_string()));
        self.items
            .sort_by(|a, b| DirectoryList::compare_dir_items(a, b, &self.sort_by));

        if self.state.selected() == None {
            self.state.select(Some(0));
        }

        Ok(())
    }

    /// Sort the DirectoryListItems based on the `sort_by` parameter.
    fn compare_dir_items(a: &DirectoryListItem, b: &DirectoryListItem, sort_by: &SortBy) -> Ordering {
        match (a, b) {
            (DirectoryListItem::ParentDir(a_str), DirectoryListItem::ParentDir(b_str)) => {
                a_str.cmp(b_str)
            }
            (DirectoryListItem::ParentDir(_), DirectoryListItem::Entry(_)) => Ordering::Less,
            (DirectoryListItem::Entry(_), DirectoryListItem::ParentDir(_)) => Ordering::Greater,
            (DirectoryListItem::Entry(a), DirectoryListItem::Entry(b)) => {
                #[allow(unused_assignments)]
                let mut sort_by_direction = &SortByDirection::default();
                let mut retval = match sort_by {
                    SortBy::TypeName(direction) => {
                        sort_by_direction = direction;
                        if a.file_type.is_dir() && !b.file_type.is_dir() {
                            Ordering::Less
                        } else if !a.file_type.is_dir() && b.file_type.is_dir() {
                            Ordering::Greater
                        } else {
                            a.name.cmp(&b.name)
                        }
                    }
                    SortBy::Size(direction) => {
                        sort_by_direction = direction;
                        if a.size < b.size {
                            Ordering::Less
                        } else if a.size > b.size {
                            Ordering::Greater
                        } else {
                            Ordering::Equal
                        }
                    }
                    SortBy::Name(direction) => {
                        sort_by_direction = direction;
                        a.name.cmp(&b.name)
                    }
                    SortBy::DateTime(direction) => {
                        sort_by_direction = direction;
                        a.modified.cmp(&b.modified)
                    }
                };
                // reverse the order?
                match sort_by_direction {
                    SortByDirection::Asc => {}
                    SortByDirection::Dec => retval = retval.reverse(),
                }
                retval
            }
        }
    }

    /// Select the first DirectoryListItem with the given name.
    /// If none exists, nothing will be selected.
    pub(crate) fn select_by_name(&mut self, name: &str) {
        self.unselect();
        for (i, x) in self.items.iter().enumerate() {
            match x {
                DirectoryListItem::Entry(entry) => {
                    let fname = entry.name.to_string();
                    if name.eq(fname.as_str()) {
                        self.state.select(Some(i));
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    /// Select the next item in the list, without wrapping.
    pub(crate) fn select_next(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i < self.items.len() - 1 {
                    i + 1
                } else {
                    self.items.len() - 1
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    /// Select the previous item in the list, without wrapping.
    pub(crate) fn select_previous(&mut self) {
        let i = match self.state.selected() {
            Some(i) => {
                if i > 0 {
                    i - 1
                } else {
                    0
                }
            }
            None => 0,
        };
        self.state.select(Some(i));
    }

    pub(crate) fn select_last(&mut self) {
        if self.items.len() > 0 {
            self.state.select(Some(self.items.len() - 1));
        }
    }

    pub(crate) fn select_first(&mut self) {
        if self.items.len() > 0 {
            self.state.select(Some(0));
        }
    }

    /// Unselect any previously selected item in the list.
    fn unselect(&mut self) {
        self.state.select(None);
    }
}

