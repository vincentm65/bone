//! Windows named-pipe listener, in place of the Unix socket.

use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};

use bone_proto::transport;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::Server;

/// A pipe name such as `\\.\pipe\bone3-USER` that we serve. The name goes
/// away with the last instance.
pub struct Listener {
    path: PathBuf,
    next: std::sync::Mutex<Option<NamedPipeServer>>,
    /// Who may connect: this user, at any integrity (see [`Access`]).
    access: Access,
}

impl Listener {
    /// Create the first instance of `path`. A pipe another server already
    /// serves is an error.
    pub async fn bind(path: &Path) -> io::Result<Self> {
        let access = Access::for_this_user()?;
        let first = access
            .create(ServerOptions::new().first_pipe_instance(true).reject_remote_clients(true), path)
            .map_err(|e| {
                if e.raw_os_error() == Some(5) {
                    // ERROR_ACCESS_DENIED: the first instance exists.
                    io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("a server is already listening on {}", path.display()),
                    )
                } else {
                    e
                }
            })?;
        Ok(Listener {
            path: path.to_owned(),
            next: std::sync::Mutex::new(Some(first)),
            access,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A named pipe has no file to remove: new connections keep reaching
    /// this server until it exits. See the Unix listener's `unlink`.
    pub fn unlink(&self) {}

    /// Accept connections forever, serving each on its own task. A new
    /// instance waits before each client is handed off, so none is turned
    /// away between connections.
    pub async fn run(&self, server: &Server) -> io::Result<()> {
        self.run_counted(server, tokio::sync::watch::channel(0).0)
            .await
    }

    /// Like [`run`](Self::run), keeping `clients` at the number of
    /// connections currently being served.
    pub async fn run_counted(
        &self,
        server: &Server,
        clients: tokio::sync::watch::Sender<usize>,
    ) -> io::Result<()> {
        let mut pipe = match self.next.lock().unwrap().take() {
            Some(pipe) => pipe,
            None => self.instance()?,
        };
        loop {
            pipe.connect().await?;
            let connected = std::mem::replace(&mut pipe, self.instance()?);
            let (r, w) = tokio::io::split(connected);
            let server = server.clone();
            clients.send_modify(|n| *n += 1);
            let clients = clients.clone();
            tokio::spawn(async move {
                server.serve(transport::framed(r, w)).await;
                clients.send_modify(|n| *n -= 1);
            });
        }
    }

    fn instance(&self) -> io::Result<NamedPipeServer> {
        self.access
            .create(ServerOptions::new().reject_remote_clients(true), &self.path)
    }
}

/// The pipe's security descriptor: full access for the user running bone
/// (by their SID, so an elevated and a normal bone of one user both match)
/// and SYSTEM, labelled Medium integrity. A pipe gets its creator's label
/// otherwise: one made by an administrator bone (High) refuses a normal
/// bone (Medium) with "Access is denied".
struct Access(*mut c_void);

// SAFETY: the descriptor is only read (by CreateNamedPipe) after it is made.
unsafe impl Send for Access {}
unsafe impl Sync for Access {}

impl Access {
    fn for_this_user() -> io::Result<Self> {
        use windows_sys::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        let sid = user_sid()?;
        let sddl = format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)S:(ML;;NW;;;ME)");
        let wide: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut sd = std::ptr::null_mut();
        // SAFETY: a NUL-terminated string in; the descriptor out is freed in Drop.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Access(sd))
    }

    fn create(&self, options: &mut ServerOptions, path: &Path) -> io::Result<NamedPipeServer> {
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        let mut attrs = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        };
        // SAFETY: `attrs` and the descriptor it points to outlive the call.
        unsafe {
            options.create_with_security_attributes_raw(path, (&raw mut attrs).cast::<c_void>())
        }
    }
}

impl Drop for Access {
    fn drop(&mut self) {
        // SAFETY: made by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { windows_sys::Win32::Foundation::LocalFree(self.0) };
    }
}

/// The SID of the user this process runs as, as a string ("S-1-5-21-…").
fn user_sid() -> io::Result<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: plain Win32 calls on our own process token; every buffer is
    // sized as the call asks, and what they allocate is freed here.
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        // u64s keep the buffer aligned for TOKEN_USER.
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let got = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
        CloseHandle(token);
        if got == 0 {
            return Err(io::Error::last_os_error());
        }
        let user = &*(buf.as_ptr().cast::<TOKEN_USER>());
        let mut wide = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut wide) == 0 {
            return Err(io::Error::last_os_error());
        }
        let n = (0..).take_while(|&i| *wide.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(wide, n));
        LocalFree(wide.cast());
        Ok(sid)
    }
}
