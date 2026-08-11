//! Jorge rust_extender TCP simple_server — :7878, `<TCP_MESSAGE>` framing.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::thread;

use parking_lot::Mutex;

static LISTEN_FD: AtomicI32 = AtomicI32::new(-1);
static CLIENT_FD: AtomicI32 = AtomicI32::new(-1);
// Atomic (not Mutex): read by the game thread every frame, written on the server thread —
// a contended parking_lot lock can park a waiter forever in this environment.
static CLIENT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static STARTED: AtomicBool = AtomicBool::new(false);
static OUTBOX: Mutex<Vec<String>> = Mutex::new(Vec::new());
static INBOUND: Mutex<Vec<String>> = Mutex::new(Vec::new());
static RECV_BUF: Mutex<String> = Mutex::new(String::new());
static mut SOCKET_POOL: [u8; 0x40000] = [0; 0x40000];

const AF_INET: i32 = 2;
const SOCK_STREAM: i32 = 1;
const IPPROTO_TCP: i32 = 6;
const SOL_SOCKET: i32 = 0xffff;
const SO_REUSEADDR: i32 = 4;

extern "C" {
    #[link_name = "\u{1}_ZN2nn6socket4RecvEiPvmi"]
    fn Recv(socket: i32, buffer: *mut u8, bufferLength: u64, flags: i32) -> i64;
}

pub fn start(port: u16) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::slight::diag::note(format!("SRV start(:{port}) — spawning server_loop"));
    thread::spawn(move || server_loop(port));
    // Dedicated sender: the ONLY place blocking Send() runs. Keeps it off the game thread so a
    // slow RPM can never stall a frame (it just backs up in OUTBOX). See `queue`.
    thread::spawn(sender_loop);
}

/// Sleep via nn::os directly — std::thread::sleep never wakes under skyline on Ryujinx
/// (observed: both server threads went permanently silent after their first sleep; the
/// skyline panic handler also uses nn::os::SleepThread, not std).
fn sleep_ms(ms: u64) {
    unsafe {
        nnsdk::nn::os::SleepThread(nnsdk::nn::TimeSpan {
            nanoseconds: ms * 1_000_000,
        });
    }
}

fn sender_loop() {
    crate::slight::diag::note("SND thread entered");
    loop {
        let fd = CLIENT_FD.load(Ordering::Acquire);
        if fd >= 0 {
            unsafe { drain(fd) };
        }
        sleep_ms(4);
    }
}

/// Messages queued and not yet sent (diagnostic — growth = sender thread dead/stuck).
pub fn outbox_depth() -> usize {
    OUTBOX.lock().len()
}

fn server_loop(port: u16) {
    unsafe {
        crate::slight::diag::note("SRV thread entered — waiting for first game frame");
        // Touching nn::socket BEFORE it is initialized crashes/hangs this thread, and whether
        // skyline's logger has initialized it yet is a boot-order race (lost on some boots).
        // The per-frame driver ticking means boot init — including skyline's socket init — is
        // done, so gate on that before the first socket call.
        while !crate::slight::agent_extender::driver_has_ticked() {
            sleep_ms(250);
        }
        crate::slight::diag::note("SRV game frames running — probing sockets");
        // Do NOT call nn::socket::Initialize unconditionally: skyline's own TCP logger (:6969)
        // already initializes nn::socket in this process, and the nn SDK ABORTS on double-init
        // (on Ryujinx this silently killed the server thread before the socket was ever
        // created — :7878 never bound). Probe with Socket() first; only Initialize if sockets
        // aren't up yet (e.g. environments where the skyline logger is disabled).
        let mut listen = nnsdk::nn::socket::Socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
        crate::slight::diag::note(format!("SRV socket probe -> {listen}"));
        if listen < 0 {
            let rc = nnsdk::nn::socket::Initialize(
                std::ptr::addr_of_mut!(SOCKET_POOL).cast::<u8>(),
                0x40000,
                0x20000,
                0x20,
            );
            crate::slight::diag::note(format!("SRV nn::socket::Initialize rc={rc}"));
            if rc != 0 {
                skyline::println!("[SLight] nn::socket::Initialize failed: {rc}");
                return;
            }
            listen = nnsdk::nn::socket::Socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
            crate::slight::diag::note(format!("SRV socket retry -> {listen}"));
        }
        if listen < 0 {
            skyline::println!("[SLight] socket create failed ({listen}) — no debuggable server");
            return;
        }
        LISTEN_FD.store(listen, Ordering::Release);

        let reuse: i32 = 1;
        nnsdk::nn::socket::SetSockOpt(
            listen,
            SOL_SOCKET,
            SO_REUSEADDR,
            &reuse as *const i32 as *const u8,
            4,
        );

        let mut addr: nnsdk::nn::socket::SockAddrIn = std::mem::zeroed();
        addr.sin_len = 16;
        addr.sin_family = AF_INET as u8;
        addr.sin_port = nnsdk::nn::socket::InetHtons(port);
        addr.sin_addr = [0, 0, 0, 0];

        let bind_rc = nnsdk::nn::socket::Bind(
            listen,
            &addr as *const _ as *const nnsdk::root::sockaddr,
            16,
        );
        crate::slight::diag::note(format!("SRV bind :{port} rc={bind_rc}"));
        if bind_rc != 0 {
            report_bind_failure(port, bind_rc);
            return;
        }

        nnsdk::nn::socket::Listen(listen, 2);
        crate::slight::diag::note(format!("SRV listening on :{port}"));
        skyline::println!("[SLight] Accepting clients from debbugable server on :{port}");

        loop {
            let mut len: u32 = 16;
            let mut peer: nnsdk::nn::socket::SockAddrIn = std::mem::zeroed();
            let client = nnsdk::nn::socket::Accept(
                listen,
                &mut peer as *mut _ as *mut nnsdk::root::sockaddr,
                &mut len,
            ) as i32;
            if client < 0 {
                continue;
            }
            RECV_BUF.lock().clear();
            CLIENT_FD.store(client, Ordering::SeqCst);
            let cid = handshake(client);
            crate::rust_extender::debuggable_server::on_rpm_client_connected(cid);
            // Inbound only. Recv blocks until data/disconnect; outbound is the sender thread's job.
            while CLIENT_FD.load(Ordering::Acquire) == client {
                recv_once(client);
            }
        }
    }
}

/// The one line a reader should be able to grep `diag.txt` for. Kept short and unmistakable so
/// it survives being pasted out of a chat log; the deploy script prints the same phrase.
pub const DOUBLE_PLUGIN_BANNER: &str = "!!!! SLIGHT: PORT ALREADY BOUND — A SECOND COPY OF THIS \
                                        PLUGIN IS PROBABLY LOADED";

/// Say out loud what a failed bind almost always means, because the silent version of this cost
/// about six rounds of misdiagnosis.
///
/// Skyline loads **every file** in `romfs:/skyline/plugins/` as a plugin, extension ignored, so a
/// `lib_effect_viewer.nro.bak` left beside the real one runs a second full copy of us: two sets of
/// ACMD hooks, two per-frame drivers, and two servers racing for this port. The visible symptom is
/// a hard 60→30 fps drop on entering training mode, which looks like a performance regression in
/// whatever was last changed and is not one.
///
/// The bind failure was already logged — as `SRV bind :7878 rc=-1`, one lowercase line among
/// thousands, which nobody reads as "you have two plugins installed". What makes this loud is not
/// the flush; it is naming the cause, the remedy, and a second check the reader can run.
///
/// **The second check is the honest part.** A bind can fail for other reasons (a port set in
/// `gateway.txt` that something else owns), so the banner does not assert the diagnosis, it points
/// at the corroborating evidence: each instance runs its own server thread and its own diag flush,
/// so `SRV thread entered` appears **once per loaded copy**. Two of them is the proof.
///
/// It also explains the tell that misled us the first time. [`diag::start_session`] truncates and
/// rewrites the header, and both copies do it during plugin init, so whichever loads *second* wins
/// the `build=` line — a reader who just deployed build X sees build Y at the top of the file and
/// concludes their deploy did not take.
///
/// [`diag::start_session`]: crate::slight::diag::start_session
fn report_bind_failure(port: u16, rc: u32) {
    use crate::slight::diag;
    let build = crate::slight::effect_viewer::live_eff::BUILD_TAG;
    diag::note(DOUBLE_PLUGIN_BANNER);
    diag::note(format!(
        "     bind :{port} failed rc={rc}; this copy is build={build}"
    ));
    diag::note("     check: `SRV thread entered` appears once per loaded copy — two = two plugins");
    diag::note("     note:  the build= header is written by whichever copy loads LAST, not yours");
    diag::note("     fix:   romfs:/skyline/plugins/ must hold ONE lib_effect_viewer.nro and no");
    diag::note("            copies of it under any other name — Skyline loads every file there,");
    diag::note("            .bak and .old included. Re-run the deploy script; it now refuses.");
    diag::note("     without this server there is no live preview; the game is otherwise fine.");
    // Flush here rather than leaving it to the per-frame driver. This runs once, on the server
    // thread, and the buffer it would otherwise sit in is capped at 40000 lines and shared with
    // two chatty instances — the one message that must not be dropped should not queue behind
    // 30 frames of SPAWN lines from a plugin the reader does not know is running.
    diag::flush();
    skyline::println!("[SLight] bind :{port} failed rc={rc} — second plugin copy? build={build}");
}

unsafe fn handshake(client: i32) -> u64 {
    let cid = CLIENT_ID.fetch_add(1, Ordering::SeqCst) + 1;
    send_frame(client, r#"{"header":"RemoveAll","body":"{}"}"#);
    send_frame(
        client,
        &format!(
            r#"{{"header":"GiveClientId","body":"{{\"GiveClientId\":{{\"client_id\":{cid}}}}}"}}"#
        ),
    );
    cid
}

unsafe fn send_frame(client: i32, inner: &str) -> i64 {
    let msg = format!("<TCP_MESSAGE>{inner}</TCP_MESSAGE>");
    nnsdk::nn::socket::Send(client, msg.as_ptr(), msg.len() as u64, 0) as i64
}

unsafe fn recv_once(client: i32) {
    let mut chunk = [0u8; 4096];
    let n = Recv(client, chunk.as_mut_ptr(), chunk.len() as u64, 0);
    if n <= 0 {
        if n == 0 {
            CLIENT_FD.store(-1, Ordering::SeqCst);
        }
        return;
    }
    let text = String::from_utf8_lossy(&chunk[..n as usize]);
    let mut buf = RECV_BUF.lock();
    buf.push_str(&text);
    extract_inbound(&mut buf);
}

fn extract_inbound(buf: &mut String) {
    loop {
        let start = buf.find("<TCP_MESSAGE>");
        let end = buf.find("</TCP_MESSAGE>");
        if let (Some(s), Some(e)) = (start, end) {
            if e >= s {
                let payload = buf[s + 13..e].trim().to_string();
                *buf = buf[e + 14..].to_string();
                if !payload.is_empty() {
                    inbound_push(payload);
                }
                continue;
            }
        }

        if let Some(nl) = buf.find('\n') {
            let line = buf[..nl].trim().to_string();
            *buf = buf[nl + 1..].to_string();
            if !line.is_empty() {
                inbound_push(line);
            }
            continue;
        }

        // Runaway guard: only discard when we're NOT mid-message. A large frame in flight
        // (e.g. a ~1.3 MB base64 donor eff) legitimately exceeds any small cap before its
        // closing tag arrives — clearing it here silently dropped donor_bytes. Keep
        // accumulating while an opening tag is present; only clear genuine garbage.
        if buf.len() > 64 * 1024 * 1024 && !buf.contains("<TCP_MESSAGE>") {
            buf.clear();
        }
        break;
    }
}

/// Server thread → INBOUND. try_lock + SleepThread retry — never parks (parked waiters
/// never wake in this environment).
fn inbound_push(payload: String) {
    // Timing rules are pure shared state. Install them on receipt so the next ACMD coroutine
    // boundary can see a frame-0 edit; the general parser below remains responsible for all
    // commands and game-thread-only updates.
    if crate::rust_extender::debuggable_server::apply_timing_rules_from_network(&payload) {
        return;
    }
    loop {
        if let Some(mut q) = INBOUND.try_lock() {
            q.push(payload);
            return;
        }
        sleep_ms(1);
    }
}

pub fn queue(inner: String) {
    // Push only — the sender thread does the (blocking) Send. Never send from here: this is
    // called on the game thread, and a blocking Send while RPM is slow would stall the frame.
    // try_lock + short spin — NEVER park the game thread (parked waiters never wake here).
    // Worst case the message is dropped; RPM gets a full re-sync on reconnect anyway.
    for _ in 0..1000 {
        if let Some(mut out) = OUTBOX.try_lock() {
            // Bound growth if RPM is stuck; drop newest.
            if out.len() < 8192 {
                out.push(inner);
            }
            return;
        }
        core::hint::spin_loop();
    }
}

unsafe fn drain(client: i32) {
    // Take the batch out under the lock, then release before the blocking sends so `queue`
    // (game thread) never waits on the socket via the OUTBOX lock. Sender thread may sleep.
    let msgs: Vec<String> = loop {
        if let Some(mut out) = OUTBOX.try_lock() {
            break out.drain(..).collect();
        }
        sleep_ms(1);
    };
    if !msgs.is_empty() {
        crate::slight::diag::note(format!("SND drain {} msgs", msgs.len()));
    }
    for msg in msgs {
        let rc = send_frame(client, &msg);
        if rc < 0 {
            crate::slight::diag::note(format!("SND send failed rc={rc}"));
        }
    }
}

pub fn has_client() -> bool {
    CLIENT_FD.load(Ordering::Acquire) >= 0
}

pub fn client_id() -> u64 {
    CLIENT_ID.load(Ordering::Acquire)
}

/// Game thread. try_lock — under contention with the server thread's push, skip a frame
/// instead of parking (parked waiters never wake here).
pub fn take_inbound() -> Vec<String> {
    match INBOUND.try_lock() {
        Some(mut q) => q.drain(..).collect(),
        None => Vec::new(),
    }
}
