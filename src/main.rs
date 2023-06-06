use std::collections::HashMap;
use std::time::Duration;

use async_std::io::{self, prelude::*};
use async_std::net::{TcpListener, TcpStream};
use async_std::prelude::*;
use async_std::sync::{Arc, Mutex};
use async_std::task;
use iperf3::{iperf_command, SessionConfig, SessionData};
use log::{debug, info};
use once_cell::sync::Lazy;

static SESSIONS: Lazy<Arc<Mutex<HashMap<String, SessionConfig>>>> =
    Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

static SESSION_DATA: Lazy<Arc<Mutex<HashMap<String, SessionData>>>> =
    Lazy::new(|| Arc::new(Mutex::new(HashMap::new())));

fn main() -> io::Result<()> {
    env_logger::init();
    task::block_on(async {
        // All connections will come in on port 5201 by default
        // switch this to 0.0.0.0:5201 to be able to connect to this server from another machine
        let listener = TcpListener::bind("127.0.0.1:5201").await?;
        info!("Listening on {}", listener.local_addr()?);

        let mut incoming = listener.incoming();

        while let Some(stream) = incoming.next().await {
            let stream = stream?;
            task::spawn(async {
                process(stream).await.unwrap();
            });
        }
        Ok(())
    })
}

async fn process(stream: TcpStream) -> io::Result<()> {
    let peer_addr = stream.peer_addr()?;
    info!("Accepted from: {}", peer_addr);
    let mut buf_reader = stream.clone();
    let mut buf_writer = stream;
    // The first thing an iperf3 client will send is
    // a 36 character, nul terminated session id cookie
    let mut session_id_cstr: [u8; 37] = [0; 37];
    let mut session_id: [u8; 36] = [0; 36];
    buf_reader.read_exact(&mut session_id_cstr).await?;
    // The string is a C style nul terminated thing, so strip that off.
    session_id.copy_from_slice(&session_id_cstr[..36]);

    // Both client and server need the session cookie, decode it here.
    let cookie = if session_id_cstr.is_ascii() {
        Some(core::str::from_utf8(&session_id).unwrap())
    } else {
        None
    };

    if cookie.is_none() {
        // TODO: better error handling here
        info!(
            "Invalid session cookie, dropping connection from {}",
            peer_addr
        );
        return Ok(());
    }
    // Cookie must be Some() now, unwrap it to save checking every time
    let cookie = cookie.unwrap();
    let sess = SESSIONS.clone();

    // If this is the first we've seen this session cookie, assume we're the control channel
    let control_channel = {
        let cookie = String::from(cookie);
        let existing_session = sess.lock().await.contains_key(&cookie);
        if !existing_session {
            debug!("control channel opened");
            // need to disable Nagle algorithm to ensure commands are sent promptly
            buf_writer.set_nodelay(true).unwrap();

            debug!("ask the client to send the config parameters");
            buf_writer
                .write_all(&[iperf_command::PARAM_EXCHANGE])
                .await?;

            let mut message_len: [u8; 4] = [0; 4];
            buf_reader.read_exact(&mut message_len).await?;
            let message_len = u32::from_be_bytes(message_len);
            debug!("Config JSON length {}", message_len);
            let mut buf: Vec<u8> = vec![0u8; message_len as usize];
            buf_reader.read_exact(&mut buf).await?;
            if buf.is_ascii() {
                let string = String::from_utf8(buf.to_vec()).unwrap();
                debug!("Config JSON string: {}", string);
                let s: SessionConfig = serde_json::from_str(string.as_str())?;
                debug!("Config JSON decoded by serde: {:?}", s);
                if !s.tcp.unwrap_or(false) {
                    debug!("Only TCP mode is supported - dropping connection");
                    return Ok(());
                }
                if s.parallel != 1 {
                    debug!("This program does not support iperf parallel client streams, dropping this request")
                }
                if s.bidirectional.unwrap_or(false) {
                    SESSION_DATA.lock().await.insert(
                        cookie.clone(),
                        SessionData {
                            num_senders: s.parallel,
                            num_receivers: s.parallel,
                        },
                    );
                }
                sess.lock().await.insert(cookie, s);
            }
            true
        } else {
            false
        }
    };

    if control_channel {
        debug!("ask the client to connect to a 2nd socket");
        buf_writer
            .write_all(&[iperf_command::CREATE_STREAMS])
            .await?;

        // TODO: wait for connections, then send test start
        // for now, a sleep will suffice
        task::sleep(Duration::from_secs(1)).await;
        debug!("ask the client to start the test");
        buf_writer.write_all(&[iperf_command::TEST_START]).await?;

        // should probably wait for some data on the other channel for this
        debug!("tell the client we've started running the test");
        buf_writer.write_all(&[iperf_command::TEST_RUNNING]).await?;

        // the client should eventually reply with a command
        let mut reply: [u8; 1] = [0; 1];
        buf_reader.read_exact(&mut reply).await?;

        // We're hoping that it was TEST_END. check:
        if reply[0] == iperf_command::TEST_END {
            debug!("TEST_END command received from client");
            // can't exchange results, we haven't calculated them.
            // buf_writer.write_all(&[iperf_command::EXCHANGE_RESULTS]);

            buf_writer
                .write_all(&[iperf_command::DISPLAY_RESULTS])
                .await?;
            let sess = SESSIONS.clone();

            debug!("dropping session cookie now");
            sess.lock().await.remove(&String::from(cookie));
        } else {
            debug!("were expecting TEST_END, got {}", reply[0]);
        }

        // Should be done now, check:
        let mut reply: [u8; 1] = [0; 1];
        buf_reader.read_exact(&mut reply).await?;
        if reply[0] == iperf_command::IPERF_DONE {
            debug!("IPERF_DONE received from client.");
        } else {
            debug!("were expecting IPERF_DONE, got {}", reply[0]);
        }

        // Collect any data remaining in the channel, it's about to close
        let mut buf: Vec<u8> = vec![];
        let _ = buf_reader.read_to_end(&mut buf).await;
        if !buf.is_empty() {
            debug!("Printing out any remaining data in control channel...");
            if buf.is_ascii() {
                let string = String::from_utf8(buf).unwrap();
                debug!("buf: {}", string);
            } else {
                debug!("buf: {:?}", buf);
            }
        }
        debug!("control channel is done");
    } else {
        // this will be the second connection from the client
        debug!("data channel opened");
        let mut message: [u8; 0x1000] = [0; 0x1000];

        // unpack configuration data now so we don't hold this lock too long
        let (bidir, reverse) = {
            match sess.lock().await.get(&String::from(cookie)) {
                Some(config) => {
                    let bidir = config.bidirectional.unwrap_or(false);
                    let reverse = config.reverse.unwrap_or(false);
                    (bidir, reverse)
                }
                // todo: bail instead
                None => (false, false),
            }
        };
        let stream_mode = if bidir {
            let sess_type: iperf3::DataStreamType =
                if let Some(x) = SESSION_DATA.lock().await.get_mut(cookie) {
                    if x.num_receivers == 1 {
                        x.num_receivers = 0;
                        // We need to set up the receiver first, or this doesn't work. TODO: find out why.
                        iperf3::DataStreamType::Receiver
                    } else if x.num_senders == 1 {
                        x.num_senders = 0;
                        iperf3::DataStreamType::Sender
                    } else {
                        iperf3::DataStreamType::None
                    }
                } else {
                    iperf3::DataStreamType::None
                };
            match sess_type {
                iperf3::DataStreamType::Sender => {
                    debug!("Bidir Sender online");
                }
                iperf3::DataStreamType::Receiver => {
                    debug!("Bidir Receiver online");
                }
                iperf3::DataStreamType::None => {
                    debug!("FIXME: Unexpected stream type");
                }
            }
            sess_type
        } else if reverse {
            iperf3::DataStreamType::Sender
        } else {
            iperf3::DataStreamType::Receiver
        };
        match stream_mode {
            iperf3::DataStreamType::Sender => {
                // Reverse mode - send data to client
                let mut bytes_total: u64 = 0;
                let mut done = false;
                while !done {
                    match buf_writer.write(&message).await {
                        Ok(sz) => bytes_total += sz as u64,
                        Err(_) => done = true,
                    }
                }
                let gb_total = bytes_total as f32 / (1024f32 * 1024f32 * 1024f32);
                let gbit_sec = gb_total * 8f32 / 10f32;
                info!(
                    "we're done sending. sent {} bytes ({}GB), {} GBits/sec",
                    bytes_total, gb_total, gbit_sec
                );
            }
            iperf3::DataStreamType::Receiver => {
                // Forward mode - receive data from client
                let mut bytes_total: u64 = 0;
                let mut done = false;
                while !done {
                    let sz = buf_reader.read(&mut message).await?;
                    if sz == 0 {
                        done = true;
                    }
                    bytes_total += sz as u64;
                }
                let gb_total = bytes_total as f32 / (1024f32 * 1024f32 * 1024f32);
                let gbit_sec = gb_total * 8f32 / 10f32;
                info!(
                    "we're done receiving. received {} bytes ({}GB), {} GBits/sec",
                    bytes_total, gb_total, gbit_sec
                );
            }
            iperf3::DataStreamType::None => {
                //
                info!("Invalid mode, handle this!");
            }
        }
    };

    Ok(())
}
