//! The string a [`Value`](crate::value::Value) holds.
//!
//! One allocation: a reference count, a length and a capacity, followed by
//! the bytes. `Arc<str>` would be one allocation too but cannot grow, so
//! `s += t` in a loop would copy the whole string every time; `Arc<String>`
//! can grow but costs two allocations per string, and most strings a program
//! makes are small and never appended to. This has both properties: cloning
//! is a reference count, and [`Text::push_str`] appends in place whenever the
//! handle is the only one — a copy for that handle alone otherwise, which is
//! all value semantics ask for (§5.1).

use std::alloc::{alloc, dealloc, handle_alloc_error, realloc, Layout};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{fence, AtomicUsize, Ordering};

#[repr(C)]
struct Header {
    count: AtomicUsize,
    len: usize,
    cap: usize,
}

pub struct Text {
    ptr: NonNull<Header>,
}

// The bytes are only written through a handle that is the only one, and the
// count is atomic, which is what `Arc` relies on as well.
unsafe impl Send for Text {}
unsafe impl Sync for Text {}

fn layout(cap: usize) -> Layout {
    let size = std::mem::size_of::<Header>().checked_add(cap).expect("string too long");
    Layout::from_size_align(size, std::mem::align_of::<Header>()).expect("string too long")
}

impl Text {
    pub fn new(text: &str) -> Text {
        Text::with_capacity(text, text.len())
    }

    /// An empty string with room for `cap` bytes, to build one in.
    pub fn empty(cap: usize) -> Text {
        Text::with_capacity("", cap)
    }

    fn with_capacity(text: &str, cap: usize) -> Text {
        let cap = cap.max(text.len());
        let layout = layout(cap);
        // SAFETY: the layout has a nonzero size (the header), and the header
        // and the bytes are written before the pointer is handed out.
        unsafe {
            let raw = alloc(layout) as *mut Header;
            let Some(ptr) = NonNull::new(raw) else { handle_alloc_error(layout) };
            raw.write(Header { count: AtomicUsize::new(1), len: text.len(), cap });
            std::ptr::copy_nonoverlapping(text.as_ptr(), Text::data(raw), text.len());
            Text { ptr }
        }
    }

    /// Where the bytes start, just past the header.
    fn data(header: *mut Header) -> *mut u8 {
        // SAFETY: the allocation always extends past the header.
        unsafe { (header as *mut u8).add(std::mem::size_of::<Header>()) }
    }

    fn header(&self) -> &Header {
        // SAFETY: the pointer is live for as long as this handle is.
        unsafe { self.ptr.as_ref() }
    }

    pub fn as_str(&self) -> &str {
        let header = self.header();
        // SAFETY: `len` bytes of valid UTF-8 were written past the header,
        // and they only change through a handle that is the only one.
        unsafe {
            let bytes = std::slice::from_raw_parts(Text::data(self.ptr.as_ptr()), header.len);
            std::str::from_utf8_unchecked(bytes)
        }
    }

    /// Append `tail`: in place when this is the only handle, otherwise into a
    /// copy that only this handle sees.
    pub fn push_str(&mut self, tail: &str) {
        if tail.is_empty() {
            return;
        }
        let header = self.header();
        let len = header.len;
        let needed = len.checked_add(tail.len()).expect("string too long");
        if header.count.load(Ordering::Acquire) != 1 {
            let mut copy = Text::with_capacity(self.as_str(), needed);
            copy.push_str(tail);
            *self = copy;
            return;
        }
        if needed > header.cap {
            let cap = needed.max(header.cap * 2).max(16);
            // SAFETY: this is the only handle, so nothing else holds the
            // pointer `realloc` may move; the old layout is the one it was
            // allocated with.
            unsafe {
                let raw = realloc(self.ptr.as_ptr() as *mut u8, layout(header.cap), layout(cap).size());
                let Some(ptr) = NonNull::new(raw as *mut Header) else { handle_alloc_error(layout(cap)) };
                self.ptr = ptr;
                (*ptr.as_ptr()).cap = cap;
            }
        }
        // SAFETY: the only handle, with `cap - len` bytes free past the end.
        unsafe {
            let raw = self.ptr.as_ptr();
            std::ptr::copy_nonoverlapping(tail.as_ptr(), Text::data(raw).add(len), tail.len());
            (*raw).len = needed;
        }
    }
}

impl fmt::Write for Text {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.push_str(text);
        Ok(())
    }
}

impl Clone for Text {
    fn clone(&self) -> Text {
        // Relaxed is enough to take another handle, as it is for `Arc`.
        self.header().count.fetch_add(1, Ordering::Relaxed);
        Text { ptr: self.ptr }
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        if self.header().count.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        fence(Ordering::Acquire);
        let cap = self.header().cap;
        // SAFETY: that was the last handle.
        unsafe { dealloc(self.ptr.as_ptr() as *mut u8, layout(cap)) }
    }
}

impl Deref for Text {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Text {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for Text {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for Text {
    fn from(text: &str) -> Text {
        Text::new(text)
    }
}

impl From<String> for Text {
    fn from(text: String) -> Text {
        Text::new(&text)
    }
}

impl PartialEq for Text {
    fn eq(&self, other: &Text) -> bool {
        self.ptr == other.ptr || self.as_str() == other.as_str()
    }
}

impl Eq for Text {}

impl PartialOrd for Text {
    fn partial_cmp(&self, other: &Text) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Text {
    fn cmp(&self, other: &Text) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl Hash for Text {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state)
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

#[cfg(test)]
mod tests {
    use super::Text;

    #[test]
    fn appends_in_place_and_leaves_copies_alone() {
        let mut a = Text::new("ab");
        let b = a.clone();
        a.push_str("cd");
        assert_eq!(a.as_str(), "abcd");
        assert_eq!(b.as_str(), "ab");
        for _ in 0..1000 {
            a.push_str("é");
        }
        assert_eq!(a.chars().count(), 1004);
        let c = a.clone();
        drop(a);
        assert!(c.ends_with("éé"));
        assert_eq!(Text::new("").as_str(), "");
    }
}
