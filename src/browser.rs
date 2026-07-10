#![allow(non_snake_case, non_camel_case_types, unpredictable_function_pointer_comparisons)]
// Port of src/browser.c from GNU nano.
// C original: Copyright (C) 2001-2011, 2013-2026 Free Software Foundation, Inc.
//             Copyright (C) 2015, 2016, 2020, 2022, 2025 Benno Schulenberg

use crate::definitions::*;
#[allow(unused_imports)] // some of these are used only under feature gates
use crate::global::{state, state_mut, with_state, KEY_ENTER};


// ---------------------------------------------------------------------------
// Browser module-local state (replaces C file-scope statics)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
thread_local! {
    /// The list of files to display in the file browser.
    static FILELIST: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    /// The number of files in the list.
    static LIST_LENGTH: std::cell::RefCell<usize> = std::cell::RefCell::new(0);
    /// The number of screen rows we can use to display the list.
    static USABLE_ROWS: std::cell::RefCell<usize> = std::cell::RefCell::new(0);
    /// The number of files that we can display per screen row.
    static PILES: std::cell::RefCell<i32> = std::cell::RefCell::new(0);
    /// The width of a 'pile' -- the widest filename plus ten.
    static GAUGE: std::cell::RefCell<i32> = std::cell::RefCell::new(0);
    /// The currently selected filename in the list; zero-based.
    static SELECTED: std::cell::RefCell<usize> = std::cell::RefCell::new(0);
}

// Helper macros for browser statics
#[cfg(feature = "browser")]
macro_rules! bl_get {
    ($name:ident) => { $name.with(|v| *v.borrow()) };
}
#[cfg(feature = "browser")]
macro_rules! bl_set {
    ($name:ident, $val:expr) => { $name.with(|v| { *v.borrow_mut() = $val; }); };
}

// ---------------------------------------------------------------------------
// read_the_list — fill FILELIST with directory contents, set GAUGE, PILES
// C: void read_the_list(const char *path, DIR *dir)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
pub fn read_the_list(_path: &str, entries: Vec<String>) {
    use crate::utils::breadth;

    let cols = state().midwin.cols as i32;
    let editwinrows = state().editwinrows;
    let zero = state().flag_isset(ZERO);
    let lines = crate::winio::screen_rows() as i32;

    // Find the width of the widest filename in the current folder.
    let mut widest: usize = 0;
    for entry in &entries {
        // Use just the basename for width calculation.
        let name = crate::utils::tail(entry);
        let span = breadth(name);
        if span > widest {
            widest = span;
        }
    }

    // Reserve ten columns for blanks plus file size.
    let mut gauge = (widest + 10) as i32;
    // If needed, make room for ".. (parent dir)".
    if gauge < 15 { gauge = 15; }
    // Make sure we're not wider than the window.
    if gauge > cols { gauge = cols; }

    let list_length = entries.len();

    FILELIST.with(|fl| { *fl.borrow_mut() = entries; });
    bl_set!(LIST_LENGTH, list_length);
    bl_set!(GAUGE, gauge);

    // Calculate how many files fit on a line.
    // Feign room for two spaces beyond the right edge, two spaces padding between columns.
    let piles = if gauge + 2 > 0 { (cols + 2) / (gauge + 2) } else { 1 };
    bl_set!(PILES, piles.max(1));

    let usable = editwinrows
        .saturating_sub(if zero && lines > 1 { 1 } else { 0 }) as usize;
    bl_set!(USABLE_ROWS, usable.max(1));
}

// ---------------------------------------------------------------------------
// reselect — reselect the given file or directory name, if it still exists
// C: void reselect(const char *name)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
pub fn reselect(name: &str) {
    let list_length = bl_get!(LIST_LENGTH);
    let selected = bl_get!(SELECTED);

    let found = FILELIST.with(|fl| {
        let fl = fl.borrow();
        fl.iter().position(|s| s == name)
    });

    if let Some(pos) = found {
        bl_set!(SELECTED, pos);
    } else if selected > list_length {
        bl_set!(SELECTED, list_length.saturating_sub(1));
    } else {
        bl_set!(SELECTED, selected.saturating_sub(1));
    }
}

// ---------------------------------------------------------------------------
// browser_refresh — display at most a screenful of filenames
// C: void browser_refresh(void)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
pub fn browser_refresh() {
    use crate::utils::{breadth, tail, actual_x};
    use crate::winio::{titlebar, blank_edit, display_string, apply_interface_color, reset_color};
    use crossterm::{queue, cursor::MoveTo, style::Print};
    use std::io::Write;

    let present_path = state().present_path.clone();
    titlebar(present_path.as_deref());
    blank_edit();

    let selected = bl_get!(SELECTED);
    let list_length = bl_get!(LIST_LENGTH);
    let usable_rows = bl_get!(USABLE_ROWS);
    let piles = bl_get!(PILES) as usize;
    let gauge = bl_get!(GAUGE) as usize;
    let show_cursor = state().flag_isset(SHOW_CURSOR);
    let cols = state().midwin.cols as usize;
    let (midwin_y, midwin_x) = with_state(|s| (s.midwin.y, s.midwin.x));
    let selected_pair = state().interface_color_pair[SELECTED_TEXT];

    let mut row: usize = 0;
    let mut col: usize = 0;
    let mut the_row: usize = 0;
    let mut the_col: usize = 0;

    let start_index = selected - selected % (usable_rows * piles);

    let filelist_snapshot: Vec<String> = FILELIST.with(|fl| fl.borrow().clone());

    let mut stdout = crate::winio::out();

    let mut index = start_index;
    while index < list_length && row < usable_rows {
        let filepath = &filelist_snapshot[index];
        let thename = tail(filepath);
        let namelen = breadth(thename);
        let infomaxlen: usize = 7;
        let dots = cols >= 15 && namelen >= gauge.saturating_sub(infomaxlen);

        // display_string: if dots, show fragment with offset; else show from start
        let fragment_offset = if dots {
            namelen + infomaxlen + 4 - gauge
        } else {
            0
        };
        let disp = display_string(thename, fragment_offset, gauge, false, false);

        let abs_row = midwin_y + row as u16;
        let abs_col = midwin_x + col as u16;

        // If this is the selected item, draw its highlighted bar upfront.
        if index == selected {
            apply_interface_color(selected_pair);
            let _ = queue!(stdout,
                MoveTo(abs_col, abs_row),
                Print(format!("{:>width$}", " ", width = gauge)),
            );
            the_row = row;
            the_col = col;
        }

        // Print "..." prefix if name is truncated.
        if dots {
            let _ = queue!(stdout, MoveTo(abs_col, abs_row), Print("..."));
        }
        let text_col = if dots { abs_col + 3 } else { abs_col };
        let _ = queue!(stdout, MoveTo(text_col, abs_row), Print(&disp));

        let col_after_name = col + gauge;

        // Build file info string: "--" for symlink/missing, "(dir)", or size.
        let info: String = {
            use std::fs;
            let lstat_res = fs::symlink_metadata(filepath);
            let stat_res = fs::metadata(filepath);

            let is_symlink = lstat_res.as_ref().map(|m| m.file_type().is_symlink()).unwrap_or(false);

            if is_symlink || lstat_res.is_err() {
                // symlink or error
                if stat_res.as_ref().map(|m| m.is_dir()).unwrap_or(false) {
                    "(dir)".to_string()
                } else {
                    "--".to_string()
                }
            } else if stat_res.as_ref().map(|m| m.is_dir()).unwrap_or(false) {
                if thename == ".." {
                    "(parent dir)".to_string()
                } else {
                    "(dir)".to_string()
                }
            } else {
                // Regular file: show human-readable size.
                let size = stat_res.as_ref().map(|m| m.len()).unwrap_or(0);
                if size < (1 << 10) {
                    format!("{:4}  B", size)
                } else if size < (1 << 20) {
                    format!("{:4} KB", size >> 10)
                } else if size < (1 << 30) {
                    format!("{:4} MB", size >> 20)
                } else {
                    let gb = size >> 30;
                    if gb < (1 << 10) {
                        format!("{:4} GB", gb)
                    } else {
                        "(huge)".to_string()
                    }
                }
            }
        };

        // Clip info to infomaxlen columns.
        let info_maxlen = if thename == ".." { 12 } else { 7 };
        let infolen = breadth(&info);
        let info_display = if infolen > info_maxlen {
            let cut = actual_x(&info, info_maxlen);
            info[..cut].to_string()
        } else {
            info
        };
        let infolen2 = breadth(&info_display);

        let info_abs_col = midwin_x + (col_after_name - infolen2.min(col_after_name)) as u16;
        let _ = queue!(stdout, MoveTo(info_abs_col, abs_row), Print(&info_display));

        // Finish the highlight for the selected item.
        if index == selected {
            reset_color();
        }

        // Advance column.
        col += gauge + 2;
        if col > cols.saturating_sub(gauge) {
            row += 1;
            col = 0;
        }

        index += 1;
    }

    // If requested, put the cursor on the selected item and switch it on.
    if show_cursor {
        let _ = queue!(stdout,
            MoveTo(midwin_x + the_col as u16, midwin_y + the_row as u16),
        );
        // curs_set(1) equivalent — show cursor
        let _ = queue!(stdout, crossterm::cursor::Show);
    }

    let _ = stdout.flush();
}

// ---------------------------------------------------------------------------
// findfile — look for the given needle in the list of files
// C: void findfile(const char *needle, bool forwards)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
pub fn findfile(needle: &str, forwards: bool) {
    use crate::chars::mbstrcasestr;
    use crate::winio::{statusbar};
    use crate::search::not_found_msg;
    use crate::utils::tail;

    let list_length = bl_get!(LIST_LENGTH);
    if list_length == 0 {
        return;
    }

    let began_at = bl_get!(SELECTED);
    let mut selected = began_at;

    loop {
        if forwards {
            if selected == list_length - 1 {
                selected = 0;
                statusbar("Search Wrapped");
            } else {
                selected += 1;
            }
        } else {
            if selected == 0 {
                selected = list_length - 1;
                statusbar("Search Wrapped");
            } else {
                selected -= 1;
            }
        }

        // When the needle occurs in the basename of the file, we have a match.
        let name = FILELIST.with(|fl| {
            fl.borrow().get(selected).map(|s| s.clone()).unwrap_or_default()
        });
        let basename = tail(&name).to_string();
        if mbstrcasestr(&basename, needle).is_some() {
            if selected == began_at {
                statusbar("This is the only occurrence");
            }
            bl_set!(SELECTED, selected);
            return;
        }

        // When we're back at the starting point without any match.
        if selected == began_at {
            not_found_msg(needle);
            bl_set!(SELECTED, selected);
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// search_filename — prompt for and search for a filename in the browser
// C: void search_filename(bool forwards)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
pub fn search_filename(forwards: bool) {
    use crate::prompt::do_prompt;
    use crate::winio::statusbar;
    use crate::history::HistoryKind;
    use crate::utils::breadth;
    use crate::winio::display_string;

    let last_search = state().last_search.clone();
    let cols = state().midwin.cols as usize;

    // If something was searched for before, show it between square brackets.
    let thedefault: String = if !last_search.is_empty() {
        let disp = display_string(&last_search, 0, cols / 3, false, false);
        let has_more = breadth(&last_search) > cols / 3;
        format!(" [{}{}]", disp, if has_more { "..." } else { "" })
    } else {
        String::new()
    };

    let backward_label = " [Backwards]";
    let search_label = "Search";
    let msg = format!("{}{}{}", search_label, if !forwards { backward_label } else { "" }, thedefault);

    let response = do_prompt(
        MWHEREISFILE,
        Some(""),
        Some(HistoryKind::Search),
        Some(browser_refresh as fn()),
        &msg,
    );

    if response == -1 || (response == -2 && last_search.is_empty()) {
        statusbar("Cancelled");
        return;
    }

    // If the user typed an answer, remember it.
    let answer = state().answer.clone();
    if !answer.is_empty() {
        state_mut().last_search = answer.clone();
        #[cfg(feature = "histories")]
        {
            use crate::history::{update_history};
            update_history(HistoryKind::Search, &answer, PRUNE_DUPLICATE);
        }
    }

    if response == 0 || response == -2 {
        let last = state().last_search.clone();
        findfile(&last, forwards);
    }
}

// ---------------------------------------------------------------------------
// research_filename — search again without prompting
// C: void research_filename(bool forwards)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
pub fn research_filename(forwards: bool) {
    use crate::winio::{statusbar, wipe_statusbar};

    #[cfg(feature = "histories")]
    {
        let last_search = state().last_search.clone();
        if last_search.is_empty() {
            // Take the last item from history.
            let hist_last = with_state(|s| {
                s.search_history_items.last().cloned()
            });
            if let Some(item) = hist_last {
                state_mut().last_search = item;
            }
        }
    }

    let last_search = state().last_search.clone();
    if last_search.is_empty() {
        statusbar("No current search pattern");
    } else {
        wipe_statusbar();
        findfile(&last_search, forwards);
    }
}

// ---------------------------------------------------------------------------
// to_first_file — select the first file in the list
// C: void to_first_file(void)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
pub fn to_first_file() {
    bl_set!(SELECTED, 0);
}

// ---------------------------------------------------------------------------
// to_last_file — select the last file in the list
// C: void to_last_file(void)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
pub fn to_last_file() {
    let list_length = bl_get!(LIST_LENGTH);
    if list_length > 0 {
        bl_set!(SELECTED, list_length - 1);
    }
}

// ---------------------------------------------------------------------------
// strip_last_component — strip one element from the end of path
// C: char *strip_last_component(const char *path)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
pub fn strip_last_component(path: &str) -> String {
    let trimmed = path.trim_end_matches(|c| c == '/' || c == '\\');
    if trimmed.is_empty() {
        return String::new();
    }
    std::path::Path::new(trimmed)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(feature = "browser")]
fn path_join_display(base: &str, child: &str) -> String {
    std::path::Path::new(base)
        .join(child)
        .to_string_lossy()
        .into_owned()
}

#[cfg(feature = "browser")]
fn has_trailing_separator(path: &str) -> bool {
    path.ends_with('/') || path.ends_with('\\')
}

#[cfg(feature = "browser")]
fn is_root_path(path: &str) -> bool {
    std::path::Path::new(path).parent().is_none()
}

// ---------------------------------------------------------------------------
// browse — allow the user to browse through directories
// C: char *browse(char *path)
// ---------------------------------------------------------------------------
/// The `toggle` field of the shortcut bound to `kbinput` in the current menu
/// (C: get_shortcut(kbinput)->toggle).
#[cfg(all(feature = "browser", not(feature = "tiny")))]
fn shortcut_toggle_for(kbinput: i32) -> i32 {
    crate::global::with_state(|s| {
        let cm = s.currmenu;
        s.sclist.iter()
            .find(|sc| (sc.menus as u32 & cm) != 0 && sc.keycode == kbinput)
            .map(|sc| sc.toggle)
            .unwrap_or(0)
    })
}

#[cfg(feature = "browser")]
pub fn browse(initial_path: String) -> Option<String> {
    use crate::global::{with_state, interpret};
    use crate::global::{
        do_help, full_refresh, do_search_backward, do_search_forward,
        do_findprevious, do_findnext, do_left, do_right, to_prev_word, to_next_word,
        do_up, do_down, to_prev_block, to_next_block, do_page_up, do_page_down,
        do_enter, do_exit, goto_dir,
    };
    use crate::winio::{statusline, statusbar, bottombars, titlebar, edit_refresh, get_kbinput};
    use crate::files::{
        confined_is_dir, confined_read_dir, expand_leading_tilde, get_full_path,
        outside_of_confinement,
    };
    use crate::utils::tail;
    use std::path::Path;

    let mut path = initial_path;
    let mut present_name: Option<String> = None;
    let mut chosen: Option<String> = None;

    // Outer loop: re-read directory on navigation or resize.
    'reload: loop {
        // Canonicalize path.
        let full = get_full_path(&path);
        if let Some(fp) = full {
            path = fp;
        }

        let dir_entries: Result<Vec<String>, std::io::Error> = (|| {
            let mut v: Vec<String> = Vec::new();
            v.push(path_join_display(&path, ".."));
            let rd = confined_read_dir(&path)?;
            for entry in rd {
                let name = entry.to_string_lossy().to_string();
                // Skip the useless "." item.
                if name == "." { continue; }
                v.push(path_join_display(&path, &name));
            }
            Ok(v)
        })();

        match dir_entries {
            Err(e) => {
                let msg = format!("Cannot open directory: {}", e);
                statusline(MessageType::Alert, &msg);
                // If we don't have a file list, there is nothing to show.
                if bl_get!(LIST_LENGTH) == 0 {
                    state_mut().lastmessage = MessageType::Vacuum;
                    drop(present_name);
                    napms(1200);
                    return None;
                }
                // Fall back to current path.
                let prev_path = state().present_path.clone()
                    .unwrap_or_else(|| ".".to_string());
                let fallback = FILELIST.with(|fl| {
                    let sel = bl_get!(SELECTED);
                    fl.borrow().get(sel).cloned()
                });
                path = prev_path;
                present_name = fallback;
                // Re-read with the old path by continuing without new entries.
                // Since we can't open it again, just keep the old list going.
            }
            Ok(mut entries) => {
                // Sort the list.
                entries.sort_by(|a, b| crate::files::diralphasort(a, b));
                read_the_list(&path, entries);
            }
        }

        // Reselect or reset selection.
        if let Some(ref name) = present_name {
            reselect(name);
            present_name = None;
        } else {
            bl_set!(SELECTED, 0);
        }

        let mut old_selected: usize = usize::MAX;

        state_mut().present_path = Some(path.clone());
        titlebar(Some(&path));

        let list_length = bl_get!(LIST_LENGTH);
        if list_length == 0 {
            statusline(MessageType::Alert, "No entries");
            napms(1200);
            // No entries — break and return None
            break 'reload;
        }

        // A nested prompt or help viewer can consume the resize while this
        // loop is suspended.  Comparing the shared generation on return tells
        // us to rebuild the browser grid without a browser-only global flag.
        let seen_resize_generation = crate::winio::resize_generation();

        // Inner loop: handle keystrokes until a file is selected or user exits.
        loop {
            if crate::winio::consume_resize_request(None)
                || crate::winio::resize_generation() != seen_resize_generation
            {
                present_name = FILELIST.with(|fl| {
                    fl.borrow().get(bl_get!(SELECTED)).cloned()
                });
                continue 'reload;
            }

            state_mut().lastmessage = MessageType::Vacuum;

            bottombars(MBROWSER);

            let selected = bl_get!(SELECTED);
            let show_cursor = state().flag_isset(SHOW_CURSOR);

            if old_selected != selected || show_cursor {
                browser_refresh();
            }
            old_selected = selected;

            #[cfg(feature = "mouse")]
            let mut kbinput = get_kbinput(show_cursor);
            #[cfg(not(feature = "mouse"))]
            let kbinput = get_kbinput(show_cursor);

            if crate::winio::consume_resize_request(Some(kbinput)) {
                present_name = FILELIST.with(|fl| {
                    fl.borrow().get(bl_get!(SELECTED)).cloned()
                });
                continue 'reload;
            }

            #[cfg(feature = "mouse")]
            {
                use crate::winio::{get_mouseinput, KEY_MOUSE_CODE};
                if kbinput == KEY_MOUSE_CODE {
                    let mut mouse_x: i32 = 0;
                    let mut mouse_y: i32 = 0;
                    if get_mouseinput(&mut mouse_y, &mut mouse_x) == 0 {
                        // Check if click is in the midwin area.
                        let (mid_y, mid_rows) = with_state(|s| (s.midwin.y as i32, s.midwin.rows as i32));
                        if mouse_y >= mid_y && mouse_y < mid_y + mid_rows {
                            let usable_rows = bl_get!(USABLE_ROWS);
                            let piles = bl_get!(PILES) as usize;
                            let gauge = bl_get!(GAUGE) as usize;
                            let list_length = bl_get!(LIST_LENGTH);
                            let row = mouse_y - mid_y;
                            if let Some(new_sel) = browser_index_for_click(
                                selected,
                                usable_rows,
                                piles,
                                gauge,
                                list_length,
                                mouse_x,
                                row,
                            ) {
                                bl_set!(SELECTED, new_sel);

                                // Double-click: choose the file.
                                if old_selected == new_sel {
                                    kbinput = KEY_ENTER;
                                }
                            }
                        }
                    }
                    if kbinput == crate::winio::KEY_MOUSE_CODE {
                        continue;
                    }
                }
            }

            let function = interpret(kbinput);

            let list_length = bl_get!(LIST_LENGTH);
            let selected = bl_get!(SELECTED);
            let usable_rows = bl_get!(USABLE_ROWS);
            let piles = bl_get!(PILES) as usize;
            let _gauge_val = bl_get!(GAUGE) as usize;
            let _cols = state().midwin.cols as usize;

            if function == Some(do_help as crate::definitions::FuncPtr) {
                do_help();
            } else if function == Some(full_refresh as crate::definitions::FuncPtr) {
                #[cfg(feature = "tiny")]
                full_refresh();
                #[cfg(not(feature = "tiny"))]
                {
                    // Treat as resize.
                    // Remember selected file, re-read directory.
                    present_name = FILELIST.with(|fl| {
                        fl.borrow().get(selected).cloned()
                    });
                    continue 'reload;
                }
            } else if {
                #[cfg(not(feature = "tiny"))]
                {
                    function == Some(crate::global::do_toggle as crate::definitions::FuncPtr)
                        && shortcut_toggle_for(kbinput) == NO_HELP as i32
                }
                #[cfg(feature = "tiny")]
                { false }
            } {
                // M-X in the browser: toggle the help lines (C: do_toggle with
                // toggle == NO_HELP), then treat it as a resize so the browser
                // re-reads the directory and repaints.
                #[cfg(not(feature = "tiny"))]
                {
                    crate::TOGGLE!(NO_HELP);
                    crate::winio::window_init();
                    present_name = FILELIST.with(|fl| fl.borrow().get(selected).cloned());
                    continue 'reload;
                }
            } else if function == Some(do_search_backward as crate::definitions::FuncPtr) {
                search_filename(BACKWARD);
            } else if function == Some(do_search_forward as crate::definitions::FuncPtr) {
                search_filename(FORWARD);
            } else if function == Some(do_findprevious as crate::definitions::FuncPtr) {
                research_filename(BACKWARD);
            } else if function == Some(do_findnext as crate::definitions::FuncPtr) {
                research_filename(FORWARD);
            } else if function == Some(do_left as crate::definitions::FuncPtr) {
                if selected > 0 {
                    bl_set!(SELECTED, selected - 1);
                }
            } else if function == Some(do_right as crate::definitions::FuncPtr) {
                if selected < list_length - 1 {
                    bl_set!(SELECTED, selected + 1);
                }
            } else if function == Some(to_prev_word as crate::definitions::FuncPtr) {
                bl_set!(SELECTED, selected - (selected % piles));
            } else if function == Some(to_next_word as crate::definitions::FuncPtr) {
                let new_sel = selected + piles - 1 - (selected % piles);
                bl_set!(SELECTED, if new_sel >= list_length { list_length - 1 } else { new_sel });
            } else if function == Some(do_up as crate::definitions::FuncPtr) {
                if selected >= piles {
                    bl_set!(SELECTED, selected - piles);
                }
            } else if function == Some(do_down as crate::definitions::FuncPtr) {
                if selected + piles <= list_length - 1 {
                    bl_set!(SELECTED, selected + piles);
                }
            } else if function == Some(to_prev_block as crate::definitions::FuncPtr) {
                let new_sel = (selected / (usable_rows * piles)) * usable_rows * piles
                    + selected % piles;
                bl_set!(SELECTED, new_sel);
            } else if function == Some(to_next_block as crate::definitions::FuncPtr) {
                let mut new_sel = (selected / (usable_rows * piles)) * usable_rows * piles
                    + selected % piles + usable_rows * piles - piles;
                if new_sel >= list_length {
                    new_sel = (list_length / piles) * piles + selected % piles;
                }
                if new_sel >= list_length {
                    new_sel -= piles;
                }
                bl_set!(SELECTED, new_sel);
            } else if function == Some(do_page_up as crate::definitions::FuncPtr) {
                let new_sel = if selected < piles {
                    0
                } else if selected < usable_rows * piles {
                    selected % piles
                } else {
                    selected - usable_rows * piles
                };
                bl_set!(SELECTED, new_sel);
            } else if function == Some(do_page_down as crate::definitions::FuncPtr) {
                let new_sel = if selected + piles >= list_length.saturating_sub(1) {
                    list_length - 1
                } else if selected + usable_rows * piles >= list_length {
                    (selected + usable_rows * piles - list_length) % piles + list_length - piles
                } else {
                    selected + usable_rows * piles
                };
                bl_set!(SELECTED, new_sel);
            } else if function == Some(to_first_file as crate::definitions::FuncPtr) {
                to_first_file();
            } else if function == Some(to_last_file as crate::definitions::FuncPtr) {
                to_last_file();
            } else if function == Some(goto_dir as crate::definitions::FuncPtr) {
                use crate::prompt::do_prompt;
                // Ask for the directory to go to.
                let r = do_prompt(
                    MGOTODIR,
                    Some(""),
                    None,
                    Some(browser_refresh as fn()),
                    "Go To Directory",
                );
                if r < 0 {
                    statusbar("Cancelled");
                    // Fall through to testresize check.
                } else {
                    let answer = state().answer.clone();
                    let mut new_path = expand_leading_tilde(&answer);

                    // If the given path is relative, join it with the current path.
                    if !Path::new(&new_path).is_absolute() {
                        let cur_path = state().present_path.clone()
                            .unwrap_or_default();
                        new_path = path_join_display(&cur_path, &answer);
                    }

                    #[cfg(feature = "operatingdir")]
                    {
                        let operating_dir = state().operating_dir.clone();
                        if let Some(ref opdir) = operating_dir {
                            if outside_of_confinement(&new_path, false) {
                                let msg = format!("Can't go outside of {}", opdir);
                                statusline(MessageType::Alert, &msg);
                                path = state().present_path.clone()
                                    .unwrap_or_else(|| ".".to_string());
                                // goto testresize — fall through
                                if crate::winio::resize_generation() != seen_resize_generation {
                                    present_name = FILELIST.with(|fl| {
                                        fl.borrow().get(bl_get!(SELECTED)).cloned()
                                    });
                                    continue 'reload;
                                }
                                continue;
                            }
                        }
                    }

                    // Snip any trailing slashes.
                    while has_trailing_separator(&new_path) && !is_root_path(&new_path) {
                        new_path.pop();
                    }

                    // Select the specified path in the current list if present.
                    FILELIST.with(|fl| {
                        let fl = fl.borrow();
                        if let Some(pos) = fl.iter().position(|s| s == &new_path) {
                            bl_set!(SELECTED, pos);
                        }
                    });

                    path = new_path;
                    continue 'reload;
                }
            } else if function == Some(do_enter as crate::definitions::FuncPtr) {
                let selected_file = FILELIST.with(|fl| {
                    fl.borrow().get(selected).cloned()
                }).unwrap_or_default();

                // Can't move up from root.
                if tail(&selected_file) == ".." && is_root_path(&path) {
                    statusline(MessageType::Alert, "Can't move up a directory");
                    continue;
                }

                #[cfg(feature = "operatingdir")]
                {
                    let operating_dir = state().operating_dir.clone();
                    if let Some(ref opdir) = operating_dir {
                        if outside_of_confinement(&selected_file, false) {
                            let msg = format!("Can't go outside of {}", opdir);
                            statusline(MessageType::Alert, &msg);
                            continue;
                        }
                    }
                }

                // If for some reason the file is inaccessible, complain.
                match confined_is_dir(&selected_file) {
                    Err(e) => {
                        let msg = format!("Error reading {}: {}", selected_file, e);
                        statusline(MessageType::Alert, &msg);
                        continue;
                    }
                    Ok(is_directory) => {
                        if !is_directory {
                            // A file was selected — we're done.
                            chosen = Some(selected_file);
                            break 'reload;
                        }
                        // Moving into a directory.
                        // If moving up one level, remember where we came from.
                        let basename = tail(&selected_file).to_string();
                        if basename == ".." {
                            present_name = Some(strip_last_component(&selected_file));
                        }
                        path = selected_file;
                        continue 'reload;
                    }
                }
            } else if function == Some(do_exit as crate::definitions::FuncPtr) {
                break 'reload;
            } else {
                // Check for implant (nanorc string bind).
                #[cfg(feature = "nanorc")]
                {
                    use crate::winio::implant;
                    
                    if let Some(func) = function {
                        // The C code: implant(first_sc_for(MBROWSER, function)->expansion)
                        // We look up the expansion and call implant.
                        if let Some(sc_info) = with_state(|s| {
                            s.sclist.iter()
                                .find(|sc| (sc.menus as u32 & MBROWSER) != 0 && sc.func == Some(func))
                                .and_then(|sc| sc.expansion.clone())
                        }) {
                            implant(&sc_info);
                        } else {
                            unbound_key_stub(kbinput);
                        }
                    } else {
                        unbound_key_stub(kbinput);
                    }
                }
                #[cfg(not(feature = "nanorc"))]
                {
                    if function.is_none() {
                        unbound_key_stub(kbinput);
                    }
                }
            }

            // Handle paste input (not tiny).
            #[cfg(not(feature = "tiny"))]
            if kbinput == START_OF_PASTE as i32 {
                // Drain until END_OF_PASTE.
                loop {
                    let k = get_kbinput(false);
                    if k == END_OF_PASTE as i32 { break; }
                }
                statusline(MessageType::Ahem, "Paste is ignored");
            }

            // testresize: handle terminal resize.
            if crate::winio::resize_generation() != seen_resize_generation {
                present_name = FILELIST.with(|fl| {
                    fl.borrow().get(bl_get!(SELECTED)).cloned()
                });
                continue 'reload;
            }
        } // inner loop
    } // 'reload loop

    titlebar(None);
    edit_refresh();

    // Clean up filelist.
    FILELIST.with(|fl| { fl.borrow_mut().clear(); });
    bl_set!(LIST_LENGTH, 0);

    chosen
}

/// Translate a browser-relative click into an entry index.
///
/// Coordinates stay signed until their bounds have been checked, preventing a
/// click to the left of the list from wrapping into a huge `usize`.
#[cfg(feature = "browser")]
fn browser_index_for_click(
    selected: usize,
    usable_rows: usize,
    piles: usize,
    gauge: usize,
    list_length: usize,
    mouse_x: i32,
    mouse_y: i32,
) -> Option<usize> {
    if mouse_x < 0 || mouse_y < 0 || usable_rows == 0 || piles == 0 || list_length == 0 {
        return None;
    }

    let row = mouse_y as usize;
    if row >= usable_rows {
        return None;
    }

    let page_size = usable_rows.checked_mul(piles)?;
    let cell_width = gauge.checked_add(2)?;
    let x = mouse_x as usize;
    let base = selected - selected % page_size;
    let col = x / cell_width;
    let mut new_selected = base
        .checked_add(row.checked_mul(piles)?)?
        .checked_add(col)?;

    // Preserve nano's behavior in the padding beyond the final pile: select
    // the preceding entry rather than jumping to the next logical row.
    if x > piles.saturating_mul(cell_width) {
        new_selected = new_selected.saturating_sub(1);
    }

    Some(new_selected.min(list_length - 1))
}

// ---------------------------------------------------------------------------
// browse_in — prepare to start browsing at the given path
// C: char *browse_in(const char *inpath)
// ---------------------------------------------------------------------------
#[cfg(feature = "browser")]
pub fn browse_in(inpath: &str) -> Option<String> {
    use crate::files::{confined_is_dir, outside_of_confinement, expand_leading_tilde};

    let mut path = expand_leading_tilde(inpath);

    // If path is not a directory, try to strip a filename from it.
    let needs_strip = confined_is_dir(&path)
        .map(|is_directory| !is_directory)
        .unwrap_or(true);

    if needs_strip {
        path = strip_last_component(&path);

        let still_not_dir = confined_is_dir(&path)
            .map(|is_directory| !is_directory)
            .unwrap_or(true);

        if still_not_dir {
            
            let cwd = std::env::current_dir().ok()
                .and_then(|p| p.to_str().map(|s| s.to_string()));
            match cwd {
                Some(p) => path = p,
                None => {
                    use crate::winio::statusline;
                    statusline(MessageType::Alert, "The working directory has disappeared");
                    napms(1200);
                    return None;
                }
            }
        }
    }

    #[cfg(feature = "operatingdir")]
    {
        if let Some(ref opdir) = state().operating_dir.clone() {
            if outside_of_confinement(&path, false) {
                path = opdir.clone();
            }
        }
    }

    browse(path)
}

// ---------------------------------------------------------------------------
// napms — sleep in milliseconds (lets flash messages linger to be read)
// ---------------------------------------------------------------------------
#[inline]
fn napms(ms: i32) {
    crate::winio::napms(ms.max(0) as u64);
}

// ---------------------------------------------------------------------------
// unbound_key_stub — local stub for unbound key handling
// ---------------------------------------------------------------------------
fn unbound_key_stub(kbinput: i32) {
    // Calls the winio unbound key display (stub until winio.rs has the real one)
    use crate::winio::statusline;
    // In C: unbound_key(kbinput) — just show a message
    let msg = format!("Unknown command: {:#x}", kbinput);
    statusline(MessageType::Ahem, &msg);
}

#[cfg(all(test, feature = "browser"))]
mod tests {
    use super::browser_index_for_click;

    #[test]
    fn browser_click_rejects_negative_and_out_of_view_coordinates() {
        assert_eq!(browser_index_for_click(0, 3, 2, 10, 20, -1, 0), None);
        assert_eq!(browser_index_for_click(0, 3, 2, 10, 20, 0, -1), None);
        assert_eq!(browser_index_for_click(0, 3, 2, 10, 20, 0, 3), None);
    }

    #[test]
    fn browser_click_uses_window_relative_x_without_editor_margin() {
        // The current selection is on the second page (six entries per page).
        // A click at raw x=5, row=1 selects the first pile on that row.
        assert_eq!(browser_index_for_click(7, 3, 2, 10, 20, 5, 1), Some(8));
    }

    #[test]
    fn browser_click_clamps_to_last_entry() {
        assert_eq!(browser_index_for_click(7, 3, 2, 10, 9, 12, 1), Some(8));
        assert_eq!(browser_index_for_click(7, 3, 2, 10, 9, 1000, 2), Some(8));
    }
}
