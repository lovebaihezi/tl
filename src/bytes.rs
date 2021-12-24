use core::{fmt, fmt::Debug};
use std::{
    borrow::Cow,
    hash::{Hash, Hasher},
    marker::PhantomData,
    mem::ManuallyDrop,
};

/// A wrapper around raw bytes
#[derive(Eq, PartialOrd, Ord)]
pub struct Bytes<'a> {
    /// The inner data
    data: BytesInner,
    /// Enforce the lifetime of the referenced data
    _lt: PhantomData<&'a [u8]>,
}

/// The inner data of [`Bytes`]
///
/// Instead of using `&[u8]` and `Vec<u8>` for the variants,
/// we use raw pointers and a `u32` for the length.
/// This is to keep the size of the enum to 16 (on 64-bit machines),
/// which is the same as if this was just `struct Bytes<'a>(&'a [u8])`
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum BytesInner {
    /// Borrowed bytes
    Borrowed(*const u8, u32),
    /// Owned bytes
    Owned(*mut u8, u32),
}

impl<'a> PartialEq for Bytes<'a> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        let this = self.as_bytes();
        let that = other.as_bytes();
        this == that
    }
}

impl<'a> Hash for Bytes<'a> {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Hash must be implemented manually for Bytes, otherwise it would only hash a pointer
        let this = self.as_bytes();
        this.hash(state);
    }
}

impl<'a> Clone for Bytes<'a> {
    fn clone(&self) -> Self {
        // It is important to manually implement Clone for Bytes,
        // because if `self` was owned, then the default clone
        // implementation would only clone the pointer
        // which leads to aliasing boxes, and later, when `Bytes` is dropped,
        // the box is freed twice!
        match &self.data {
            BytesInner::Borrowed(data, len) => {
                Bytes::from(unsafe { compact_bytes_to_slice(*data, *len) })
            }
            BytesInner::Owned(data, len) => {
                let (ptr, len) = unsafe { clone_compact_bytes_parts(*data, *len) };
                Bytes {
                    data: BytesInner::Owned(ptr, len),
                    _lt: PhantomData,
                }
            }
        }
    }
}

impl<'a> From<&'a str> for Bytes<'a> {
    #[inline]
    fn from(s: &'a str) -> Self {
        <Self as From<&'a [u8]>>::from(s.as_bytes())
    }
}

impl<'a> From<&'a [u8]> for Bytes<'a> {
    #[inline]
    fn from(s: &'a [u8]) -> Self {
        Bytes {
            data: BytesInner::Borrowed(s.as_ptr(), s.len() as u32),
            _lt: PhantomData,
        }
    }
}

/// Converts `Bytes` raw parts to a slice
#[inline]
unsafe fn compact_bytes_to_slice<'a>(ptr: *const u8, l: u32) -> &'a [u8] {
    std::slice::from_raw_parts(ptr, l as usize)
}

/// Converts `Bytes` raw parts to a boxed slice
#[inline]
unsafe fn compact_bytes_to_boxed_slice(ptr: *mut u8, len: u32) -> Box<[u8]> {
    let len = len as usize;

    // carefully reconstruct a `Box<[u8]>` from the raw pointer and length
    Vec::from_raw_parts(ptr, len, len).into_boxed_slice()
}

/// Converts a boxed byte slice to compact raw parts
///
/// The caller is responsible for freeing the returned pointer
unsafe fn boxed_slice_to_compact_parts(slice: Box<[u8]>) -> (*mut u8, u32) {
    // wrap box in `ManuallyDrop` so it's not dropped at the end of the scope
    let mut slice = ManuallyDrop::new(slice);
    let len = slice.len();
    let ptr = slice.as_mut_ptr();

    (ptr, len as u32)
}

/// Clones compact byte parts and returns the new parts
#[inline]
unsafe fn clone_compact_bytes_parts(ptr: *mut u8, len: u32) -> (*mut u8, u32) {
    boxed_slice_to_compact_parts(compact_bytes_to_boxed_slice(ptr, len).clone())
}

// Custom `Debug` trait is implemented which displays the data as a UTF8 string,
// to make it easier to read for humans when logging
impl<'a> Debug for Bytes<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Bytes").field(&self.as_utf8_str()).finish()
    }
}

impl<'a> Bytes<'a> {
    /// Convenient method for lossy-encoding the data as UTF8
    #[inline]
    pub fn as_utf8_str(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(self.as_bytes())
    }

    /// Returns the raw data wrapped by this struct
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        match &self.data {
            BytesInner::Borrowed(b, l) => unsafe { compact_bytes_to_slice(*b, *l) },
            BytesInner::Owned(o, l) => unsafe { compact_bytes_to_slice(*o, *l) },
        }
    }

    /// Returns the raw data referenced by this struct
    ///
    /// The lifetime of the returned data is tied to 'a, unlike `Bytes::as_bytes`
    /// which has a lifetime of '_ (self) in case it is owned
    #[inline]
    pub fn as_bytes_borrowed(&self) -> Option<&'a [u8]> {
        match &self.data {
            BytesInner::Borrowed(b, l) => Some(unsafe { compact_bytes_to_slice(*b, *l) }),
            _ => None,
        }
    }

    /// Returns a read-only raw pointer to the inner data
    #[inline]
    pub fn as_ptr(&self) -> *const u8 {
        match &self.data {
            BytesInner::Borrowed(b, _) => *b,
            BytesInner::Owned(o, _) => *o,
        }
    }

    /// Sets the inner data to the given bytes
    pub fn set<B: Into<Box<[u8]>>>(&mut self, data: B) -> Result<(), SetBytesError> {
        const MAX: usize = u32::MAX as usize;

        let data = <B as Into<Box<[u8]>>>::into(data);

        if data.len() > MAX {
            return Err(SetBytesError::LengthOverflow);
        }

        // SAFETY: All invariants are checked
        unsafe { self.set_unchecked(data) };
        Ok(())
    }

    /// Sets the inner data to the given bytes without checking for validity of the data
    ///
    /// ## Safety
    /// - Once `data` is converted to a `Box<[u8]>`, its length must not be greater than u32::MAX
    #[inline]
    pub unsafe fn set_unchecked<B: Into<Box<[u8]>>>(&mut self, data: B) {
        let data = <B as Into<Box<[u8]>>>::into(data);

        let (ptr, len) = boxed_slice_to_compact_parts(data);

        self.data = BytesInner::Owned(ptr, len);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SetBytesError {
    /// The length of the given data would overflow a `u32`
    LengthOverflow,
}

impl Drop for BytesInner {
    fn drop(&mut self) {
        // we only need to deallocate if we own the data
        // if we don't, just do nothing
        if let BytesInner::Owned(ptr, len) = self {
            let ptr = *ptr;
            let len = *len as usize;

            // carefully reconstruct a `Box<[u8]>` from the raw pointer and length
            // and immediately drop it to free memory
            unsafe { drop(Vec::from_raw_parts(ptr, len, len).into_boxed_slice()) };
        }
    }
}
