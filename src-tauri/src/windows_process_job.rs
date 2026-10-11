//! Windows Job Object ownership for HTTPS MCP child process trees.
//! Closing the final job handle kills the bridge, its stdio server and SSH.
use std::{
    io, mem,
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::{Child, Command}, ptr,
};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next,
            TH32CS_SNAPTHREAD, THREADENTRY32,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
            JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
        Threading::{OpenThread, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME},
    },
};

pub(super) struct KillOnCloseJob {
    handle: HANDLE,
}

impl KillOnCloseJob {
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: unnamed job, null security attributes and name are valid.
        let handle = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: Windows defines this as a POD C struct; all-zero fields
        // represent disabled limits until explicitly enabled below.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: the handle is owned by this function; the structure and its
        // size match the JobObjectExtendedLimitInformation API contract.
        let result = unsafe {
            SetInformationJobObject(
                handle, JobObjectExtendedLimitInformation,
                &raw const info as *const _, mem::size_of_val(&info) as u32,
            )
        };
        if result == 0 {
            let error = io::Error::last_os_error();
            unsafe { CloseHandle(handle); }
            return Err(error);
        }
        Ok(Self { handle })
    }

    /// Keep the primary thread suspended until the process belongs to the
    /// kill-on-close Job Object. Otherwise it could spawn untracked children
    /// in the interval between Command::spawn() and assignment.
    pub(super) fn spawn(command: &mut Command) -> io::Result<(Child, Self)> {
        let job = Self::new()?;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        if let Err(error) = job.assign(&child)
            .and_then(|_| resume_primary_thread(child.id())) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok((child, job))
    }

    pub(super) fn assign(&self, child: &Child) -> io::Result<()> {
        // SAFETY: Child owns an open process handle and self owns an active
        // job handle; Windows adds this process and its future descendants.
        let result = unsafe {
            AssignProcessToJobObject(self.handle, child.as_raw_handle() as HANDLE)
        };
        if result == 0 { return Err(io::Error::last_os_error()); }
        Ok(())
    }
}

fn resume_primary_thread(process_id: u32) -> io::Result<()> {
    // A newly created CREATE_SUSPENDED process has exactly one thread.
    // ToolHelp obtains its ID without needing CreateProcessW's thread handle,
    // which std::process::Child does not expose.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32::default();
    entry.dwSize = mem::size_of::<THREADENTRY32>() as u32;
    let mut found = None;
    let mut ok = unsafe { Thread32First(snapshot, &mut entry) };
    while ok != 0 {
        if entry.th32OwnerProcessID == process_id {
            found = Some(entry.th32ThreadID);
            break;
        }
        ok = unsafe { Thread32Next(snapshot, &mut entry) };
    }
    unsafe { CloseHandle(snapshot); }
    let thread_id = found.ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "Cannot find suspended process thread")
    })?;
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if thread.is_null() { return Err(io::Error::last_os_error()); }
    let previous_count = unsafe { ResumeThread(thread) };
    unsafe { CloseHandle(thread); }
    if previous_count != 1 {
        return Err(io::Error::other(format!(
            "Unexpected suspended process thread count: {previous_count}"
        )));
    }
    Ok(())
}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // SAFETY: handle is non-null and owned exclusively by this value.
        // The kill-on-close policy terminates any remaining descendants.
        unsafe { CloseHandle(self.handle); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{process::{Command, Stdio}, thread, time::Duration};
    use windows_sys::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::Threading::WaitForSingleObject,
    };
    #[test]
    fn closing_job_terminates_windows_child() {
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c", "ping -n 30 127.0.0.1 >NUL"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let (child, job) = KillOnCloseJob::spawn(&mut command).expect("spawn inside job");
        let handle = child.as_raw_handle() as HANDLE;
        drop(job);
        // SAFETY: Child retains a valid process handle until the end of this test.
        let result = unsafe { WaitForSingleObject(handle, 5_000) };
        assert_eq!(result, WAIT_OBJECT_0);
        thread::sleep(Duration::from_millis(20));
    }

    #[test]
    fn suspended_child_cannot_execute_before_assignment() {
        let job = KillOnCloseJob::new().expect("create job");
        let mut command = Command::new("cmd.exe");
        command.args(["/d", "/c", "exit /b 33"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        let mut child = command.spawn().expect("suspended spawn");
        thread::sleep(Duration::from_millis(100));
        assert!(child.try_wait().unwrap().is_none(),
            "child must remain suspended until job assignment");
        job.assign(&child).expect("assign before execution");
        resume_primary_thread(child.id()).expect("resume assigned process");
        let exit = child.wait().expect("wait child");
        assert_eq!(exit.code(), Some(33));
    }
}
