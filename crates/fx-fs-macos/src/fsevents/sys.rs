//! Raw FSEvents, CoreFoundation and libdispatch declarations (macOS only).
//!
//! Written from Apple's FSEvents documentation and the SDK headers
//! `CoreServices/FSEvents.h`, `CoreFoundation/CFBase.h`, `CFArray.h`,
//! `CFString.h`, `CFUUID.h`, `CFRunLoop.h` and `dispatch/queue.h`. Names
//! follow the headers so they can be checked against them.
//!
//! Everything here is `unsafe` to call; the safe wrappers live in
//! [`super::volume`] and the stream module.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]

use std::ffi::{c_char, c_void};
use std::marker::{PhantomData, PhantomPinned};

/// Declares an opaque C type that can only be used behind a pointer.
macro_rules! opaque {
    ($($name:ident),* $(,)?) => {$(
        #[repr(C)]
        pub struct $name {
            _data: [u8; 0],
            _marker: PhantomData<(*mut u8, PhantomPinned)>,
        }
    )*};
}

opaque!(
    FSEventStreamOpaque,
    CFAllocatorOpaque,
    CFArrayOpaque,
    CFStringOpaque,
    CFUUIDOpaque,
    CFRunLoopOpaque,
    DispatchQueueOpaque,
    DispatchQueueAttrOpaque,
);

// ------------------------------------------------------------ CoreFoundation

/// `CFIndex` (`signed long`).
pub type CFIndex = isize;
/// `Boolean` (`unsigned char`).
pub type Boolean = u8;
/// `CFTimeInterval` (seconds).
pub type CFTimeInterval = f64;
/// `CFAbsoluteTime`: seconds since 2001-01-01 00:00:00 UTC.
pub type CFAbsoluteTime = f64;
/// `CFStringEncoding`.
pub type CFStringEncoding = u32;
/// `CFTypeRef`.
pub type CFTypeRef = *const c_void;
/// `CFAllocatorRef` (null means the default allocator).
pub type CFAllocatorRef = *const CFAllocatorOpaque;
/// `CFArrayRef`.
pub type CFArrayRef = *const CFArrayOpaque;
/// `CFStringRef`.
pub type CFStringRef = *const CFStringOpaque;
/// `CFUUIDRef`.
pub type CFUUIDRef = *const CFUUIDOpaque;
/// `CFRunLoopRef`.
pub type CFRunLoopRef = *mut CFRunLoopOpaque;
/// `CFRunLoopMode` (a `CFStringRef`).
pub type CFRunLoopMode = CFStringRef;

/// `kCFAbsoluteTimeIntervalSince1970`: seconds from the Unix epoch to the
/// CoreFoundation epoch (2001-01-01).
pub const kCFAbsoluteTimeIntervalSince1970: CFTimeInterval = 978_307_200.0;
/// `kCFStringEncodingUTF8`.
pub const kCFStringEncodingUTF8: CFStringEncoding = 0x0800_0100;

/// `kCFRunLoopRunFinished`.
pub const kCFRunLoopRunFinished: i32 = 1;
/// `kCFRunLoopRunStopped`.
pub const kCFRunLoopRunStopped: i32 = 2;
/// `kCFRunLoopRunTimedOut`.
pub const kCFRunLoopRunTimedOut: i32 = 3;
/// `kCFRunLoopRunHandledSource`.
pub const kCFRunLoopRunHandledSource: i32 = 4;

/// `CFUUIDBytes`: sixteen `UInt8` fields, returned by value.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CFUUIDBytes {
    pub byte0: u8,
    pub byte1: u8,
    pub byte2: u8,
    pub byte3: u8,
    pub byte4: u8,
    pub byte5: u8,
    pub byte6: u8,
    pub byte7: u8,
    pub byte8: u8,
    pub byte9: u8,
    pub byte10: u8,
    pub byte11: u8,
    pub byte12: u8,
    pub byte13: u8,
    pub byte14: u8,
    pub byte15: u8,
}

impl CFUUIDBytes {
    /// The sixteen bytes in order.
    #[must_use]
    pub const fn to_array(self) -> [u8; 16] {
        [
            self.byte0,
            self.byte1,
            self.byte2,
            self.byte3,
            self.byte4,
            self.byte5,
            self.byte6,
            self.byte7,
            self.byte8,
            self.byte9,
            self.byte10,
            self.byte11,
            self.byte12,
            self.byte13,
            self.byte14,
            self.byte15,
        ]
    }
}

/// `CFArrayCallBacks`.
#[repr(C)]
pub struct CFArrayCallBacks {
    pub version: CFIndex,
    pub retain: Option<unsafe extern "C" fn(CFAllocatorRef, *const c_void) -> *const c_void>,
    pub release: Option<unsafe extern "C" fn(CFAllocatorRef, *const c_void)>,
    pub copyDescription: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
    pub equal: Option<unsafe extern "C" fn(*const c_void, *const c_void) -> Boolean>,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    /// `kCFTypeArrayCallBacks`: retain/release callbacks for arrays of CF
    /// objects.
    pub static kCFTypeArrayCallBacks: CFArrayCallBacks;
    /// `kCFRunLoopDefaultMode`.
    pub static kCFRunLoopDefaultMode: CFRunLoopMode;

    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFRelease(cf: CFTypeRef);

    pub fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;

    pub fn CFStringCreateWithBytes(
        alloc: CFAllocatorRef,
        bytes: *const u8,
        numBytes: CFIndex,
        encoding: CFStringEncoding,
        isExternalRepresentation: Boolean,
    ) -> CFStringRef;
    pub fn CFStringCreateWithFileSystemRepresentation(
        alloc: CFAllocatorRef,
        buffer: *const c_char,
    ) -> CFStringRef;
    pub fn CFStringGetLength(theString: CFStringRef) -> CFIndex;
    pub fn CFStringGetMaximumSizeOfFileSystemRepresentation(string: CFStringRef) -> CFIndex;
    pub fn CFStringGetFileSystemRepresentation(
        string: CFStringRef,
        buffer: *mut c_char,
        maxBufLen: CFIndex,
    ) -> Boolean;

    pub fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const *const c_void,
        numValues: CFIndex,
        callBacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;
    pub fn CFArrayGetCount(theArray: CFArrayRef) -> CFIndex;
    pub fn CFArrayGetValueAtIndex(theArray: CFArrayRef, idx: CFIndex) -> *const c_void;

    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopRun();
    pub fn CFRunLoopRunInMode(
        mode: CFRunLoopMode,
        seconds: CFTimeInterval,
        returnAfterSourceHandled: Boolean,
    ) -> i32;
    pub fn CFRunLoopStop(rl: CFRunLoopRef);
    pub fn CFRunLoopWakeUp(rl: CFRunLoopRef);
}

// ------------------------------------------------------------ libdispatch

/// `dispatch_queue_t`.
pub type dispatch_queue_t = *mut DispatchQueueOpaque;
/// `dispatch_queue_attr_t` (null means a serial queue).
pub type dispatch_queue_attr_t = *mut DispatchQueueAttrOpaque;

// libdispatch is part of libSystem, which every macOS binary links.
unsafe extern "C" {
    pub fn dispatch_queue_create(
        label: *const c_char,
        attr: dispatch_queue_attr_t,
    ) -> dispatch_queue_t;
    /// `dispatch_release(dispatch_object_t)`; pass the queue cast to a
    /// pointer.
    pub fn dispatch_release(object: *mut c_void);
    /// `dispatch_sync_f`: runs `work(context)` on `queue` and waits for it.
    /// On a serial queue this also waits for every block queued before it.
    pub fn dispatch_sync_f(
        queue: dispatch_queue_t,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
}

// ------------------------------------------------------------ FSEvents

/// `FSEventStreamRef`.
pub type FSEventStreamRef = *mut FSEventStreamOpaque;
/// `ConstFSEventStreamRef`.
pub type ConstFSEventStreamRef = *const FSEventStreamOpaque;
/// `FSEventStreamEventId`.
pub type FSEventStreamEventId = u64;
/// `FSEventStreamEventFlags`.
pub type FSEventStreamEventFlags = u32;
/// `FSEventStreamCreateFlags`.
pub type FSEventStreamCreateFlags = u32;

/// `kFSEventStreamEventIdSinceNow`.
pub const kFSEventStreamEventIdSinceNow: FSEventStreamEventId = 0xFFFF_FFFF_FFFF_FFFF;

/// `kFSEventStreamCreateFlagNone`.
pub const kFSEventStreamCreateFlagNone: FSEventStreamCreateFlags = 0x0000_0000;
/// `kFSEventStreamCreateFlagUseCFTypes`: `eventPaths` is a `CFArrayRef` of
/// `CFStringRef` instead of a C array of C strings.
pub const kFSEventStreamCreateFlagUseCFTypes: FSEventStreamCreateFlags = 0x0000_0001;
/// `kFSEventStreamCreateFlagNoDefer`: deliver the first event of a quiet
/// period immediately instead of after `latency`.
pub const kFSEventStreamCreateFlagNoDefer: FSEventStreamCreateFlags = 0x0000_0002;
/// `kFSEventStreamCreateFlagWatchRoot`: report changes to the watched
/// path itself (`RootChanged`).
pub const kFSEventStreamCreateFlagWatchRoot: FSEventStreamCreateFlags = 0x0000_0004;
/// `kFSEventStreamCreateFlagIgnoreSelf`.
pub const kFSEventStreamCreateFlagIgnoreSelf: FSEventStreamCreateFlags = 0x0000_0008;
/// `kFSEventStreamCreateFlagFileEvents`: file-level instead of
/// directory-level events.
pub const kFSEventStreamCreateFlagFileEvents: FSEventStreamCreateFlags = 0x0000_0010;
/// `kFSEventStreamCreateFlagMarkSelf`.
pub const kFSEventStreamCreateFlagMarkSelf: FSEventStreamCreateFlags = 0x0000_0020;

/// `FSEventStreamContext`.
#[repr(C)]
pub struct FSEventStreamContext {
    /// Must be 0.
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<unsafe extern "C" fn(*const c_void)>,
    pub copyDescription: Option<unsafe extern "C" fn(*const c_void) -> CFStringRef>,
}

/// `FSEventStreamCallback`. Without `kFSEventStreamCreateFlagUseCFTypes`,
/// `eventPaths` is a `const char **` of `numEvents` C strings.
pub type FSEventStreamCallback = unsafe extern "C" fn(
    streamRef: ConstFSEventStreamRef,
    clientCallBackInfo: *mut c_void,
    numEvents: usize,
    eventPaths: *mut c_void,
    eventFlags: *const FSEventStreamEventFlags,
    eventIds: *const FSEventStreamEventId,
);

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    pub fn FSEventStreamCreate(
        allocator: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *mut FSEventStreamContext,
        pathsToWatch: CFArrayRef,
        sinceWhen: FSEventStreamEventId,
        latency: CFTimeInterval,
        flags: FSEventStreamCreateFlags,
    ) -> FSEventStreamRef;

    pub fn FSEventStreamCreateRelativeToDevice(
        allocator: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *mut FSEventStreamContext,
        deviceToWatch: libc::dev_t,
        pathsToWatchRelativeToDevice: CFArrayRef,
        sinceWhen: FSEventStreamEventId,
        latency: CFTimeInterval,
        flags: FSEventStreamCreateFlags,
    ) -> FSEventStreamRef;

    pub fn FSEventStreamGetLatestEventId(streamRef: ConstFSEventStreamRef) -> FSEventStreamEventId;
    pub fn FSEventStreamGetDeviceBeingWatched(streamRef: ConstFSEventStreamRef) -> libc::dev_t;

    pub fn FSEventsGetCurrentEventId() -> FSEventStreamEventId;
    /// Returns a +1 `CFUUIDRef` (release with `CFRelease`) or null when the
    /// device has no FSEvents store.
    pub fn FSEventsCopyUUIDForDevice(dev: libc::dev_t) -> CFUUIDRef;
    pub fn FSEventsGetLastEventIdForDeviceBeforeTime(
        dev: libc::dev_t,
        time: CFAbsoluteTime,
    ) -> FSEventStreamEventId;

    pub fn FSEventStreamScheduleWithRunLoop(
        streamRef: FSEventStreamRef,
        runLoop: CFRunLoopRef,
        runLoopMode: CFRunLoopMode,
    );
    pub fn FSEventStreamUnscheduleFromRunLoop(
        streamRef: FSEventStreamRef,
        runLoop: CFRunLoopRef,
        runLoopMode: CFRunLoopMode,
    );
    pub fn FSEventStreamSetDispatchQueue(streamRef: FSEventStreamRef, q: dispatch_queue_t);

    pub fn FSEventStreamStart(streamRef: FSEventStreamRef) -> Boolean;
    pub fn FSEventStreamFlushAsync(streamRef: FSEventStreamRef) -> FSEventStreamEventId;
    pub fn FSEventStreamFlushSync(streamRef: FSEventStreamRef);
    pub fn FSEventStreamStop(streamRef: FSEventStreamRef);
    pub fn FSEventStreamInvalidate(streamRef: FSEventStreamRef);
    pub fn FSEventStreamRelease(streamRef: FSEventStreamRef);
}
