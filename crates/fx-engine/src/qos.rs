//! Quality-of-service classes for the app's threads on macOS.
//!
//! A thread without a class runs as "legacy", which macOS schedules much
//! like work someone is waiting for: in the 0.3.1 measurement, 94% of the
//! app's CPU time ran that way, and 72% ran on the performance cores. A
//! lower class lets macOS run background work on the efficiency cores. On
//! other systems the calls do nothing.

/// The class a thread asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadClass {
    /// Work someone is waiting for, such as the search behind the window's
    /// rows.
    UserInitiated,
    /// Long-running work nobody waits on: applying file changes and saving
    /// the index. Not background, which throttles disk I/O hard: a quit
    /// waits for a save in flight.
    Utility,
}

/// Sets the calling thread's class. A refusal is ignored: the thread then
/// keeps its class.
pub fn set_current_thread_class(class: ThreadClass) {
    #[cfg(target_os = "macos")]
    {
        let class = match class {
            ThreadClass::UserInitiated => libc::qos_class_t::QOS_CLASS_USER_INITIATED,
            ThreadClass::Utility => libc::qos_class_t::QOS_CLASS_UTILITY,
        };
        // SAFETY: changes only the calling thread; the class is one of the
        // documented values and 0 is the default relative priority.
        unsafe {
            libc::pthread_set_qos_class_self_np(class, 0);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = class;
}

/// The calling thread's class, when it is one of [`ThreadClass`]; `None`
/// for any other class, such as a new thread's, or off macOS.
pub fn current_thread_class() -> Option<ThreadClass> {
    #[cfg(target_os = "macos")]
    {
        let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
        let mut priority = 0;
        // SAFETY: both out-pointers point at live locals of the expected
        // types, and `pthread_self` names the calling thread.
        let ok = unsafe {
            libc::pthread_get_qos_class_np(libc::pthread_self(), &raw mut class, &raw mut priority)
        } == 0;
        match class {
            _ if !ok => None,
            libc::qos_class_t::QOS_CLASS_USER_INITIATED => Some(ThreadClass::UserInitiated),
            libc::qos_class_t::QOS_CLASS_UTILITY => Some(ThreadClass::Utility),
            _ => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn a_thread_takes_the_class_it_asks_for() {
        for class in [ThreadClass::Utility, ThreadClass::UserInitiated] {
            let got = std::thread::spawn(move || {
                set_current_thread_class(class);
                current_thread_class()
            })
            .join()
            .expect("the thread ran");
            assert_eq!(got, Some(class));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_new_thread_has_no_class_of_ours() {
        let got = std::thread::spawn(current_thread_class)
            .join()
            .expect("the thread ran");
        assert_eq!(got, None);
    }
}
