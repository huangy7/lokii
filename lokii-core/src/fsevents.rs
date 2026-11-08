//! FSEvents 文件级事件流：从检查点事件 ID 起流时先回放历史（启动追赶），
//! 再无缝转入实时监听；事件带 ID 与 inode，供索引服务精确推进检查点、配对目录重命名。

use fsevent_sys as fs;
use fsevent_sys::core_foundation as cf;
use std::ffi::{c_void, CStr, CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

pub(crate) const ITEM_CREATED: u32 = fs::kFSEventStreamEventFlagItemCreated;
pub(crate) const ITEM_RENAMED: u32 = fs::kFSEventStreamEventFlagItemRenamed;

/// 系统当前最新的事件 ID。
pub(crate) fn current_event_id() -> u64 {
    unsafe { fs::FSEventsGetCurrentEventId() }
}



pub(crate) const MUST_SCAN_SUBDIRS: u32 = fs::kFSEventStreamEventFlagMustScanSubDirs;
pub(crate) const USER_DROPPED: u32 = fs::kFSEventStreamEventFlagUserDropped;
pub(crate) const KERNEL_DROPPED: u32 = fs::kFSEventStreamEventFlagKernelDropped;
pub(crate) const IDS_WRAPPED: u32 = fs::kFSEventStreamEventFlagEventIdsWrapped;
pub(crate) const HISTORY_DONE: u32 = fs::kFSEventStreamEventFlagHistoryDone;
pub(crate) const MOUNT_CHANGED: u32 =
    fs::kFSEventStreamEventFlagMount | fs::kFSEventStreamEventFlagUnmount;

/// 一条文件级事件；`inode` 来自扩展数据，历史事件里可能缺失。
#[derive(Debug, Clone)]
pub(crate) struct Event {
    pub path: PathBuf,
    pub flags: u32,
    pub id: u64,
    pub inode: Option<u64>,
}

const CREATE_FLAG_USE_EXTENDED_DATA: u32 = 0x40;
const NUMBER_SINT64: isize = 4;
const QOS_CLASS_USER_INITIATED: u32 = 0x19;

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn FSEventStreamSetDispatchQueue(stream: fs::FSEventStreamRef, queue: *mut c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDictionaryGetValue(dict: cf::CFRef, key: cf::CFRef) -> cf::CFRef;
    fn CFNumberGetValue(number: cf::CFRef, kind: isize, value: *mut c_void) -> u8;
    fn CFStringGetLength(string: cf::CFRef) -> cf::CFIndex;
    fn CFStringGetMaximumSizeForEncoding(length: cf::CFIndex, encoding: u32) -> cf::CFIndex;
}

extern "C" {
    fn dispatch_queue_create(label: *const std::os::raw::c_char, attr: *const c_void)
        -> *mut c_void;
    fn dispatch_queue_attr_make_with_qos_class(
        attr: *const c_void,
        qos: u32,
        relative_priority: i32,
    ) -> *const c_void;
    fn dispatch_release(object: *mut c_void);
    fn dispatch_sync_f(queue: *mut c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
}

extern "C" fn drained(_context: *mut c_void) {}

/// 扩展数据字典的两个键（头文件里是 `CFSTR` 宏，只能自己建）。
fn extended_keys() -> (cf::CFRef, cf::CFRef) {
    static KEYS: std::sync::OnceLock<(usize, usize)> = std::sync::OnceLock::new();
    let (path, inode) = *KEYS.get_or_init(|| unsafe {
        let make = |key: &CStr| {
            cf::CFStringCreateWithCString(cf::kCFAllocatorDefault, key.as_ptr(), cf::kCFStringEncodingUTF8)
                as usize
        };
        (make(c"path"), make(c"fileID"))
    });
    (path as cf::CFRef, inode as cf::CFRef)
}

unsafe fn cf_string_path(string: cf::CFRef) -> Option<PathBuf> {
    if string.is_null() {
        return None;
    }
    let capacity =
        CFStringGetMaximumSizeForEncoding(CFStringGetLength(string), cf::kCFStringEncodingUTF8) + 1;
    let mut buffer = vec![0u8; capacity as usize];
    if !cf::CFStringGetCString(
        string,
        buffer.as_mut_ptr() as *mut std::os::raw::c_char,
        capacity,
        cf::kCFStringEncodingUTF8,
    ) {
        return None;
    }
    let bytes = CStr::from_bytes_until_nul(&buffer).ok()?.to_bytes();
    Some(PathBuf::from(OsStr::from_bytes(bytes)))
}

extern "C" fn on_stream_events(
    _stream: fs::FSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    let tx = unsafe { &*(info as *const std::sync::mpsc::Sender<Vec<Event>>) };
    let (path_key, inode_key) = extended_keys();
    let batch = (0..count)
        .map(|i| unsafe {
            let entry = cf::CFArrayGetValueAtIndex(paths, i as cf::CFIndex);
            let path = cf_string_path(CFDictionaryGetValue(entry, path_key)).unwrap_or_default();
            let number = CFDictionaryGetValue(entry, inode_key);
            let mut inode = 0i64;
            let inode = (!number.is_null()
                && CFNumberGetValue(number, NUMBER_SINT64, &mut inode as *mut i64 as *mut c_void)
                    != 0)
                .then_some(inode as u64);
            Event {
                path,
                flags: *flags.add(i),
                id: *ids.add(i),
                inode,
            }
        })
        .collect();
    // 回调只拷贝后投递：在派发队列上排队太久，fseventsd 会丢事件。
    let _ = tx.send(batch);
}

extern "C" fn release_sender(info: *const c_void) {
    if !info.is_null() {
        drop(unsafe { Box::from_raw(info as *mut std::sync::mpsc::Sender<Vec<Event>>) });
    }
}

/// 运行中的流；Drop 即停止。
pub(crate) struct Stream {
    stream: fs::FSEventStreamRef,
    queue: *mut c_void,
}

// 流只会被创建、持有和停止，不会跨线程并发调用。
unsafe impl Send for Stream {}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            fs::FSEventStreamStop(self.stream);
            fs::FSEventStreamInvalidate(self.stream);
            // 串行队列上排一个空任务并等它跑完：此前已派发的回调都已结束，再释放回调用的发送端。
            dispatch_sync_f(self.queue, std::ptr::null_mut(), drained);
            fs::FSEventStreamRelease(self.stream);
            dispatch_release(self.queue);
        }
    }
}

/// 从 `since`（事件 ID，或 `kFSEventStreamEventIdSinceNow`）起监听 `roots`，事件按批投递到 `tx`。
/// 从历史 ID 起流时，先收到历史事件，再收到带 [`HISTORY_DONE`] 的哨兵，之后是实时事件。
pub(crate) fn watch(
    roots: &[PathBuf],
    since: u64,
    tx: std::sync::mpsc::Sender<Vec<Event>>,
) -> Option<Stream> {
    if roots.is_empty() {
        return None;
    }
    unsafe {
        let array = cf::CFArrayCreateMutable(cf::kCFAllocatorDefault, 0, &cf::kCFTypeArrayCallBacks);
        for root in roots {
            let Ok(c_path) = CString::new(root.as_os_str().as_bytes()) else {
                continue;
            };
            let string = cf::CFStringCreateWithCString(
                cf::kCFAllocatorDefault,
                c_path.as_ptr(),
                cf::kCFStringEncodingUTF8,
            );
            if !string.is_null() {
                cf::CFArrayAppendValue(array, string);
                cf::CFRelease(string);
            }
        }
        let context = fs::FSEventStreamContext {
            version: 0,
            info: Box::into_raw(Box::new(tx)) as *mut c_void,
            retain: None,
            release: Some(release_sender),
            copy_description: None,
        };
        let stream = fs::FSEventStreamCreate(
            cf::kCFAllocatorDefault,
            on_stream_events,
            &context,
            array,
            since,
            0.05,
            fs::kFSEventStreamCreateFlagFileEvents
                | fs::kFSEventStreamCreateFlagNoDefer
                | fs::kFSEventStreamCreateFlagUseCFTypes
                | CREATE_FLAG_USE_EXTENDED_DATA,
        );
        cf::CFRelease(array);
        if stream.is_null() {
            release_sender(context.info);
            return None;
        }
        let attr = dispatch_queue_attr_make_with_qos_class(
            std::ptr::null(),
            QOS_CLASS_USER_INITIATED,
            0,
        );
        let queue = dispatch_queue_create(c"lokii.fsevents".as_ptr(), attr);
        FSEventStreamSetDispatchQueue(stream, queue);
        let stream = Stream { stream, queue };
        (fs::FSEventStreamStart(stream.stream) != 0).then_some(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn collect_until(
        rx: &std::sync::mpsc::Receiver<Vec<Event>>,
        done: impl Fn(&[Event]) -> bool,
    ) -> Vec<Event> {
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(&events) && Instant::now() < deadline {
            if let Ok(batch) = rx.recv_timeout(Duration::from_millis(100)) {
                events.extend(batch);
            }
        }
        events
    }

    #[test]
    fn live_stream_delivers_ids_and_inodes() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let _stream = watch(&[root.clone()], fs::kFSEventStreamEventIdSinceNow, tx).expect("stream");
        std::thread::sleep(Duration::from_millis(200));
        let file = root.join("live.txt");
        std::fs::write(&file, "x").unwrap();
        let inode = std::fs::metadata(&file).unwrap().ino();

        let events = collect_until(&rx, |events| events.iter().any(|e| e.path == file));
        let event = events.iter().find(|e| e.path == file).expect("event for file");
        assert!(event.id > 0 && event.flags & ITEM_CREATED != 0, "{event:?}");
        assert_eq!(event.inode, Some(inode));
    }

    #[test]
    fn stream_from_checkpoint_replays_history_then_signals_done() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let since = current_event_id();
        let file = root.join("before.txt");
        std::fs::write(&file, "x").unwrap();
        // 给系统 fseventsd 守护进程留出足够时间将写事件提交至历史日志数据库，避免历史流开启时漏读
        std::thread::sleep(Duration::from_millis(300));

        let (tx, rx) = std::sync::mpsc::channel();
        let _stream = watch(&[root.clone()], since, tx).expect("stream");
        let events = collect_until(&rx, |events| {
            events.iter().any(|e| e.flags & HISTORY_DONE != 0)
        });
        let done = events.iter().position(|e| e.flags & HISTORY_DONE != 0);
        let seen = events.iter().position(|e| e.path == file);
        assert!(matches!((seen, done), (Some(s), Some(d)) if s < d), "{events:?}");
    }
}
