// Smoldot
// Copyright (C) 2019-2022  Parity Technologies (UK) Ltd.
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! Contains a light client implementation usable from a browser environment.

#![deny(rustdoc::broken_intra_doc_links)]
#![deny(unused_crate_dependencies)]

use core::{
    cmp::Ordering,
    ops::{Add, Sub},
    pin::Pin,
    str,
    sync::atomic,
    time::Duration,
};
use futures::prelude::*;
use smoldot_light::HandleRpcError;
use std::{
    sync::{Arc, Mutex},
    task,
};

pub mod bindings;

mod alloc;
mod cpu_rate_limiter;
mod init;
mod platform;
mod timers;

/// Uses the environment to invoke `closure` after at least `duration` has elapsed.
fn start_timer_wrap(duration: Duration, closure: impl FnOnce() + 'static) {
    let callback: Box<Box<dyn FnOnce() + 'static>> = Box::new(Box::new(closure));
    let timer_id = u32::try_from(Box::into_raw(callback) as usize).unwrap();
    // Note that ideally `duration` should be rounded up in order to make sure that it is not
    // truncated, but the precision of an `f64` is so high and the precision of the operating
    // system generally so low that this is not worth dealing with.
    unsafe { bindings::start_timer(timer_id, duration.as_secs_f64() * 1000.0) }
}

#[derive(Debug, Copy, Clone)]
pub struct Instant {
    /// Milliseconds.
    inner: f64,
}

impl PartialEq for Instant {
    fn eq(&self, other: &Instant) -> bool {
        debug_assert!(self.inner.is_finite());
        self.inner == other.inner
    }
}

// This trait is ok to implement because `self.inner` is always finite.
impl Eq for Instant {}

impl PartialOrd for Instant {
    fn partial_cmp(&self, other: &Instant) -> Option<Ordering> {
        self.inner.partial_cmp(&other.inner)
    }
}

impl Ord for Instant {
    fn cmp(&self, other: &Self) -> Ordering {
        debug_assert!(self.inner.is_finite());
        self.inner.partial_cmp(&other.inner).unwrap()
    }
}

impl Instant {
    pub fn now() -> Instant {
        let value = unsafe { bindings::monotonic_clock_ms() };
        debug_assert!(value.is_finite());
        Instant { inner: value }
    }
}

impl Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, other: Duration) -> Instant {
        let new_val = self.inner + other.as_millis() as f64;
        // Just like the `Add` implementation of the actual `Instant`, we panic if the value can't
        // be represented.
        assert!(new_val.is_finite());
        Instant { inner: new_val }
    }
}

impl Sub<Duration> for Instant {
    type Output = Instant;

    fn sub(self, other: Duration) -> Instant {
        let new_val = self.inner - other.as_millis() as f64;
        // Just like the `Sub` implementation of the actual `Instant`, we panic if the value can't
        // be represented.
        assert!(new_val.is_finite());
        Instant { inner: new_val }
    }
}

impl Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, other: Instant) -> Duration {
        let ms = self.inner - other.inner;
        assert!(ms >= 0.0);
        Duration::from_millis(ms as u64)
    }
}

static CLIENT: Mutex<Option<init::Client<platform::Platform, ()>>> = Mutex::new(None);

fn init(
    max_log_level: u32,
    enable_current_task: u32,
    cpu_rate_limit: u32,
    periodically_yield: u32,
) {
    let init_out = init::init(
        max_log_level,
        enable_current_task != 0,
        cpu_rate_limit,
        periodically_yield != 0,
    );

    let mut client_lock = crate::CLIENT.lock().unwrap();
    assert!(client_lock.is_none());
    *client_lock = Some(init_out);
}

fn set_periodically_yield(periodically_yield: u32) {
    CLIENT.lock().unwrap().as_mut().unwrap().periodically_yield = periodically_yield != 0;
}

fn start_shutdown() {
    // TODO: do this in a clean way
    std::process::exit(0)
}

fn add_chain(
    chain_spec: Vec<u8>,
    database_content: Vec<u8>,
    json_rpc_running: u32,
    potential_relay_chains: Vec<u8>,
) -> u32 {
    let mut client_lock = CLIENT.lock().unwrap();

    // Fail any new chain initialization if we're running low on memory space, which can
    // realistically happen as Wasm is a 32 bits platform. This avoids potentially running into
    // OOM errors. The threshold is completely empirical and should probably be updated
    // regularly to account for changes in the implementation.
    if alloc::total_alloc_bytes() >= usize::max_value() - 400 * 1024 * 1024 {
        let chain_id = client_lock
            .as_mut()
            .unwrap()
            .chains
            .insert(init::Chain::Erroneous {
            error:
                "Wasm node is running low on memory and will prevent any new chain from being added"
                    .into(),
        });

        return u32::try_from(chain_id).unwrap();
    }

    // Retrieve the potential relay chains parameter passed through the FFI layer.
    let potential_relay_chains: Vec<_> = {
        assert_eq!(potential_relay_chains.len() % 4, 0);
        potential_relay_chains
            .chunks(4)
            .map(|c| u32::from_le_bytes(<[u8; 4]>::try_from(c).unwrap()))
            .filter_map(|c| {
                if let Some(init::Chain::Healthy {
                    smoldot_chain_id, ..
                }) = client_lock
                    .as_ref()
                    .unwrap()
                    .chains
                    .get(usize::try_from(c).ok()?)
                {
                    Some(*smoldot_chain_id)
                } else {
                    None
                }
            })
            .collect()
    };

    // Insert the chain in the client.
    let smoldot_light::AddChainSuccess {
        chain_id: smoldot_chain_id,
        json_rpc_responses,
    } = match client_lock
        .as_mut()
        .unwrap()
        .smoldot
        .add_chain(smoldot_light::AddChainConfig {
            user_data: (),
            specification: str::from_utf8(&chain_spec)
                .unwrap_or_else(|_| panic!("non-utf8 chain spec")),
            database_content: str::from_utf8(&database_content)
                .unwrap_or_else(|_| panic!("non-utf8 database content")),
            disable_json_rpc: json_rpc_running == 0,
            potential_relay_chains: potential_relay_chains.into_iter(),
        }) {
        Ok(c) => c,
        Err(error) => {
            let chain_id = client_lock
                .as_mut()
                .unwrap()
                .chains
                .insert(init::Chain::Erroneous {
                    error: error.to_string(),
                });

            return u32::try_from(chain_id).unwrap();
        }
    };

    let outer_chain_id = client_lock
        .as_mut()
        .unwrap()
        .chains
        .insert(init::Chain::Healthy {
            smoldot_chain_id,
            json_rpc_response: None,
            json_rpc_response_info: Box::new(bindings::JsonRpcResponseInfo { ptr: 0, len: 0 }),
            json_rpc_responses_rx: None,
        });
    let outer_chain_id_u32 = u32::try_from(outer_chain_id).unwrap();

    // We wrap the JSON-RPC responses stream into a proper stream in order to be able to guarantee
    // that `poll_next()` always operates on the same future.
    let mut json_rpc_responses = json_rpc_responses.map(|json_rpc_responses| {
        stream::unfold(json_rpc_responses, |mut json_rpc_responses| async {
            // The stream ends when we remove the chain. Once the chain is removed, the user
            // cannot poll the stream anymore. Therefore it is safe to unwrap the result here.
            let msg = json_rpc_responses.next().await.unwrap();
            Some((msg, json_rpc_responses))
        })
        .boxed()
    });

    // Poll the receiver once in order for `json_rpc_responses_non_empty` to be called the first
    // time a response is received.
    if let Some(json_rpc_responses) = json_rpc_responses.as_mut() {
        let _polled_result =
            Pin::new(json_rpc_responses).poll_next(&mut task::Context::from_waker(
                &Arc::new(JsonRpcResponsesNonEmptyWaker {
                    chain_id: outer_chain_id_u32,
                })
                .into(),
            ));
        debug_assert!(_polled_result.is_pending());
    }

    if let init::Chain::Healthy {
        json_rpc_responses_rx,
        ..
    } = client_lock
        .as_mut()
        .unwrap()
        .chains
        .get_mut(outer_chain_id)
        .unwrap()
    {
        *json_rpc_responses_rx = json_rpc_responses;
    }

    outer_chain_id_u32
}

fn remove_chain(chain_id: u32) {
    let mut client_lock = CLIENT.lock().unwrap();

    match client_lock
        .as_mut()
        .unwrap()
        .chains
        .remove(usize::try_from(chain_id).unwrap())
    {
        init::Chain::Healthy {
            smoldot_chain_id,
            json_rpc_responses_rx,
            ..
        } => {
            // We've polled the JSON-RPC receiver with a waker that calls
            // `json_rpc_responses_non_empty`. Once the sender is destroyed, this waker will be
            // called in order to inform of the destruction. We don't want that to happen.
            // Therefore, we poll the receiver again with a dummy "no-op" waker for the sole
            // purpose of erasing the previously-registered waker.
            if let Some(mut json_rpc_responses_rx) = json_rpc_responses_rx {
                let _ = Pin::new(&mut json_rpc_responses_rx).poll_next(
                    &mut task::Context::from_waker(futures::task::noop_waker_ref()),
                );
            }

            let () = client_lock
                .as_mut()
                .unwrap()
                .smoldot
                .remove_chain(smoldot_chain_id);
        }
        init::Chain::Erroneous { .. } => {}
    }
}

fn chain_is_ok(chain_id: u32) -> u32 {
    let client_lock = CLIENT.lock().unwrap();
    if matches!(
        client_lock
            .as_ref()
            .unwrap()
            .chains
            .get(usize::try_from(chain_id).unwrap())
            .unwrap(),
        init::Chain::Healthy { .. }
    ) {
        1
    } else {
        0
    }
}

fn chain_error_len(chain_id: u32) -> u32 {
    let client_lock = CLIENT.lock().unwrap();
    match client_lock
        .as_ref()
        .unwrap()
        .chains
        .get(usize::try_from(chain_id).unwrap())
        .unwrap()
    {
        init::Chain::Healthy { .. } => 0,
        init::Chain::Erroneous { error } => u32::try_from(error.as_bytes().len()).unwrap(),
    }
}

fn chain_error_ptr(chain_id: u32) -> u32 {
    let client_lock = CLIENT.lock().unwrap();
    match client_lock
        .as_ref()
        .unwrap()
        .chains
        .get(usize::try_from(chain_id).unwrap())
        .unwrap()
    {
        init::Chain::Healthy { .. } => 0,
        init::Chain::Erroneous { error } => {
            u32::try_from(error.as_bytes().as_ptr() as usize).unwrap()
        }
    }
}

fn json_rpc_send(json_rpc_request: Vec<u8>, chain_id: u32) -> u32 {
    // As mentioned in the documentation, the bytes *must* be valid UTF-8.
    let json_rpc_request: String = String::from_utf8(json_rpc_request.into())
        .unwrap_or_else(|_| panic!("non-UTF-8 JSON-RPC request"));

    let mut client_lock = CLIENT.lock().unwrap();
    let client_chain_id = match client_lock
        .as_ref()
        .unwrap()
        .chains
        .get(usize::try_from(chain_id).unwrap())
        .unwrap()
    {
        init::Chain::Healthy {
            smoldot_chain_id, ..
        } => *smoldot_chain_id,
        init::Chain::Erroneous { .. } => panic!(),
    };

    match client_lock
        .as_mut()
        .unwrap()
        .smoldot
        .json_rpc_request(json_rpc_request, client_chain_id)
    {
        Ok(()) => 0,
        Err(HandleRpcError::MalformedJsonRpc(_)) => 1,
        Err(HandleRpcError::Overloaded { .. }) => 2,
    }
}

fn json_rpc_responses_peek(chain_id: u32) -> u32 {
    let mut client_lock = CLIENT.lock().unwrap();
    match client_lock
        .as_mut()
        .unwrap()
        .chains
        .get_mut(usize::try_from(chain_id).unwrap())
        .unwrap()
    {
        init::Chain::Healthy {
            json_rpc_response,
            json_rpc_responses_rx,
            json_rpc_response_info,
            ..
        } => {
            if json_rpc_response.is_none() {
                if let Some(json_rpc_responses_rx) = json_rpc_responses_rx.as_mut() {
                    loop {
                        match Pin::new(&mut *json_rpc_responses_rx).poll_next(
                            &mut task::Context::from_waker(
                                &Arc::new(JsonRpcResponsesNonEmptyWaker { chain_id }).into(),
                            ),
                        ) {
                            task::Poll::Ready(Some(response)) if response.is_empty() => {
                                // The API of `json_rpc_responses_peek` says that a length of 0
                                // indicates that the queue is empty. For this reason, we skip
                                // this response.
                                // This is a pretty niche situation, but at least we handle it
                                // properly.
                            }
                            task::Poll::Ready(Some(response)) => {
                                debug_assert!(!response.is_empty());
                                *json_rpc_response = Some(response);
                                break;
                            }
                            task::Poll::Ready(None) => unreachable!(),
                            task::Poll::Pending => break,
                        }
                    }
                }
            }

            // Note that we might be returning the last item in the queue. In principle, this means
            // that the next time an entry is added to the queue, `json_rpc_responses_non_empty`
            // should be called. Due to the way the implementation works, this will not happen
            // until the user calls `json_rpc_responses_peek`. However, this is not a problem:
            // it is impossible for the user to observe that the queue is empty, and as such there
            // is simply not correct implementation of the API that can't work because of this
            // property.

            match &json_rpc_response {
                Some(rp) => {
                    debug_assert!(!rp.is_empty());
                    json_rpc_response_info.ptr = rp.as_bytes().as_ptr() as u32;
                    json_rpc_response_info.len = rp.as_bytes().len() as u32;
                }
                None => {
                    json_rpc_response_info.ptr = 0;
                    json_rpc_response_info.len = 0;
                }
            }

            (&**json_rpc_response_info) as *const bindings::JsonRpcResponseInfo as usize as u32
        }
        _ => panic!(),
    }
}

fn json_rpc_responses_pop(chain_id: u32) {
    let mut client_lock = CLIENT.lock().unwrap();
    match client_lock
        .as_mut()
        .unwrap()
        .chains
        .get_mut(usize::try_from(chain_id).unwrap())
        .unwrap()
    {
        init::Chain::Healthy {
            json_rpc_response, ..
        } => *json_rpc_response = None,
        _ => panic!(),
    }
}

struct JsonRpcResponsesNonEmptyWaker {
    chain_id: u32,
}

impl task::Wake for JsonRpcResponsesNonEmptyWaker {
    fn wake(self: Arc<Self>) {
        unsafe { bindings::json_rpc_responses_non_empty(self.chain_id) }
    }
}

fn advance_execution() {
    let mut client_lock = CLIENT.lock().unwrap();
    let client_lock = client_lock.as_mut().unwrap();

    struct Waker {
        woken_up: atomic::AtomicBool,
    }

    impl task::Wake for Waker {
        fn wake(self: Arc<Self>) {
            self.woken_up.store(true, atomic::Ordering::Release);
        }
    }

    let waker = Arc::new(Waker {
        woken_up: atomic::AtomicBool::new(false),
    });

    loop {
        match client_lock
            .main_task
            .poll_unpin(&mut task::Context::from_waker(&waker.clone().into()))
        {
            task::Poll::Ready(infallible) => match infallible {}, // Unreachable
            task::Poll::Pending => {}
        }

        // If the task didn't wake itself up, then there is nothing left to execute immediately
        // and we break out of the loop.
        if !waker.woken_up.swap(false, atomic::Ordering::AcqRel) {
            break;
        }

        // If the task woke itself up (which means that it has more to execute), we continue
        // looping provided that `periodically_yield` is `false`.
        if !client_lock.periodically_yield {
            continue;
        }

        // If the task woke itself up and `periodically_yield` is `true`, we use
        // `setTimeout(..., 0)` to actually yield.
        start_timer_wrap(Duration::new(0, 0), advance_execution);
    }
}
