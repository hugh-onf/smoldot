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

//! Exports a function that provides bindings for the bindings found in the Rust part of the code.
//!
//! In order to use this code, call the function passing an object, then fill the `instance` field
//! of that object with the Wasm instance.

import * as buffer from './buffer.js';
import type { SmoldotWasmInstance } from './bindings.js';

export interface Config {
    instance?: SmoldotWasmInstance,

    /**
     * Array used to store the buffers provided to the Rust code.
     *
     * When `buffer_size` or `buffer_index` are called, the buffer is found here.
     */
    bufferIndices: Array<Uint8Array>,

    /**
     * Returns the number of milliseconds since an arbitrary epoch.
     */
    performanceNow: () => number,

    /**
     * Tries to open a new connection using the given configuration.
     *
     * @see Connection
     * @throws {@link ConnectionError} If the multiaddress couldn't be parsed or contains an invalid protocol.
     */
    connect(config: ConnectionConfig): Connection;
    
    /**
     * Closure to call when the Wasm instance calls `panic`.
     *
     * This callback will always be invoked from within a binding called the Wasm instance.
     */
    onPanic: (message: string) => never,
    
    logCallback: (level: number, target: string, message: string) => void,
    jsonRpcResponsesNonEmptyCallback: (chainId: number) => void,
    currentTaskCallback?: (taskName: string | null) => void,
}

/**
 * Connection to a remote node.
 *
 * At any time, a connection can be in one of the three following states:
 *
 * - `Opening` (initial state)
 * - `Open`
 * - `Reset`
 *
 * When in the `Opening` or `Open` state, the connection can transition to the `Reset` state
 * if the remote closes the connection or refuses the connection altogether. When that
 * happens, `config.onReset` is called. Once in the `Reset` state, the connection cannot
 * transition back to another state.
 *
 * Initially in the `Opening` state, the connection can transition to the `Open` state if the
 * remote accepts the connection. When that happens, `config.onOpen` is called.
 *
 * When in the `Open` state, the connection can receive messages. When a message is received,
 * `config.onMessage` is called.
 *
 * @see connect
 */
 export interface Connection {
    /**
     * Transitions the connection or one of its substreams to the `Reset` state.
     *
     * If the connection is of type "single-stream", the whole connection must be shut down.
     * If the connection is of type "multi-stream", a `streamId` can be provided, in which case
     * only the given substream is shut down.
     *
     * The `config.onReset` or `config.onStreamReset` callbacks are **not** called.
     *
     * The transition is performed in the background.
     * If the whole connection is to be shut down, none of the callbacks passed to the `Config`
     * must be called again. If only a substream is shut down, the `onStreamReset` and `onMessage`
     * callbacks must not be called again with that substream.
     */
    reset(streamId?: number): void;

    /**
     * Queues data to be sent on the given connection.
     *
     * The connection and stream must currently be in the `Open` state.
     *
     * The number of bytes most never exceed the number of "writable bytes" of the stream.
     * `onWritableBytes` can be used in order to notify that more writable bytes are available.
     *
     * The `streamId` must be provided if and only if the connection is of type "multi-stream".
     * It indicates which substream to send the data on.
     *
     * Must not be called after `closeSend` has been called.
     */
    send(data: Uint8Array, streamId?: number): void;

    /**
     * Closes the writing side of the given stream of the given connection.
     *
     * Never called for connection types where this isn't possible to implement (i.e. WebSocket
     * and WebRTC at the moment).
     *
     * The connection and stream must currently be in the `Open` state.
     *
     * Implicitly sets the "writable bytes" of the stream to zero.
     *
     * The `streamId` must be provided if and only if the connection is of type "multi-stream".
     * It indicates which substream to send the data on.
     *
     * Must only be called once per stream.
     */
    closeSend(streamId?: number): void;

    /**
     * Start opening an additional outbound substream on the given connection.
     *
     * The state of the connection must be `Open`. This function must only be called for
     * connections of type "multi-stream".
     *
     * The `onStreamOpened` callback must later be called with an outbound direction.
     * 
     * Note that no mechanism exists in this API to handle the situation where a substream fails
     * to open, as this is not supposed to happen. If you need to handle such a situation, either
     * try again opening a substream again or reset the entire connection.
     */
    openOutSubstream(): void;
}

/**
 * Configuration for a connection.
 *
 * @see connect
 */
export interface ConnectionConfig {
    /**
     * Multiaddress in string format that describes which node to try to connect to.
     *
     * Note that this address shouldn't be trusted. The value in this field might have been chosen
     * by a potentially malicious peer.
     */
    address: string,

    /**
     * Callback called when the connection transitions from the `Opening` to the `Open` state.
     *
     * Must only be called once per connection.
     */
    onOpen: (info:
        { type: 'single-stream', handshake: 'multistream-select-noise-yamux',
            initialWritableBytes: number, writeClosable: boolean
        } |
        { type: 'multi-stream', handshake: 'webrtc', 
            localTlsCertificateMultihash: Uint8Array,
            remoteTlsCertificateMultihash: Uint8Array,
        }
    ) => void;

    /**
     * Callback called when the connection transitions to the `Reset` state.
     *
     * It it **not** called if `Connection.reset` is manually called by the API user.
     */
    onConnectionReset: (message: string) => void;

    /**
     * Callback called when a new substream has been opened.
     *
     * This function must only be called for connections of type "multi-stream".
     */
    onStreamOpened: (streamId: number, direction: 'inbound' | 'outbound', initialWritableBytes: number) => void;

    /**
     * Callback called when a stream transitions to the `Reset` state.
     *
     * It it **not** called if `Connection.resetStream` is manually called by the API user.
     *
     * This function must only be called for connections of type "multi-stream".
     */
    onStreamReset: (streamId: number) => void;

    /**
     * Callback called when more data can be written on the stream.
     *
     * Can only happen while the connection is in the `Open` state.
     *
     * This callback must not be called after `closeSend` has been called.
     *
     * The `streamId` parameter must be provided if and only if the connection is of type
     * "multi-stream".
     */
    onWritableBytes: (numExtra: number, streamId?: number) => void;

    /**
     * Callback called when a message sent by the remote has been received.
     *
     * Can only happen while the connection is in the `Open` state.
     *
     * The `streamId` parameter must be provided if and only if the connection is of type
     * "multi-stream".
     */
    onMessage: (message: Uint8Array, streamId?: number) => void;
}

/**
 * Emitted by `connect` if the multiaddress couldn't be parsed or contains an invalid protocol.
 *
 * @see connect
 */
export class ConnectionError extends Error {
    constructor(message: string) {
        super(message);
    }
}

export default function (config: Config): { imports: WebAssembly.ModuleImports, killAll: () => void } {
    // Used below to store the list of all connections.
    // The indices within this array are chosen by the Rust code.
    let connections: Record<number, Connection> = {};

    // Object containing a boolean indicating whether the `killAll` function has been invoked by
    // the user.
    const killedTracked = { killed: false };

    const killAll = () => {
        killedTracked.killed = true;
        // TODO: kill timers as well?
        for (const connection in connections) {
            connections[connection]!.reset()
            delete connections[connection]
        }
    };

    const imports = {
        // Must exit with an error. A human-readable message can be found in the WebAssembly
        // memory in the given buffer.
        panic: (ptr: number, len: number) => {
            const instance = config.instance!;

            ptr >>>= 0;
            len >>>= 0;

            const message = buffer.utf8BytesToString(new Uint8Array(instance.exports.memory.buffer), ptr, len);
            config.onPanic(message);
        },

        buffer_size: (bufferIndex: number) => {
            const buf = config.bufferIndices[bufferIndex]!;
            return buf.byteLength;
        },

        buffer_copy: (bufferIndex: number, targetPtr: number) => {
            const instance = config.instance!;
            targetPtr = targetPtr >>> 0;

            const buf = config.bufferIndices[bufferIndex]!;
            new Uint8Array(instance.exports.memory.buffer).set(buf, targetPtr);
        },

        // Used by the Rust side to notify that a JSON-RPC response or subscription notification
        // is available in the queue of JSON-RPC responses.
        json_rpc_responses_non_empty: (chainId: number) => {
            if (killedTracked.killed) return;
            config.jsonRpcResponsesNonEmptyCallback(chainId);
        },

        // Used by the Rust side to emit a log entry.
        // See also the `max_log_level` parameter in the configuration.
        log: (level: number, targetPtr: number, targetLen: number, messagePtr: number, messageLen: number) => {
            if (killedTracked.killed) return;

            const instance = config.instance!;

            targetPtr >>>= 0;
            targetLen >>>= 0;
            messagePtr >>>= 0;
            messageLen >>>= 0;

            if (config.logCallback) {
                const mem = new Uint8Array(instance.exports.memory.buffer);
                let target = buffer.utf8BytesToString(mem, targetPtr, targetLen);
                let message = buffer.utf8BytesToString(mem, messagePtr, messageLen);
                config.logCallback(level, target, message);
            }
        },

        // Must return the UNIX time in milliseconds.
        unix_time_ms: () => Date.now(),

        // Must return the value of a monotonic clock in milliseconds.
        monotonic_clock_ms: () => config.performanceNow(),

        // Must call `timer_finished` after the given number of milliseconds has elapsed.
        start_timer: (id: number, ms: number) => {
            if (killedTracked.killed) return;

            const instance = config.instance!;

            // In both NodeJS and browsers, if `setTimeout` is called with a value larger than
            // 2147483647, the delay is for some reason instead set to 1.
            // As mentioned in the documentation of `start_timer`, it is acceptable to end the
            // timer before the given number of milliseconds has passed.
            if (ms > 2147483647)
                ms = 2147483647;

            // In browsers, `setTimeout` works as expected when `ms` equals 0. However, NodeJS
            // requires a minimum of 1 millisecond (if `0` is passed, it is automatically replaced
            // with `1`) and wants you to use `setImmediate` instead.
            if (ms < 1 && typeof setImmediate === "function") {
                setImmediate(() => {
                    if (killedTracked.killed) return;
                    try {
                        instance.exports.timer_finished(id);
                    } catch(_error) {}
                })
            } else {
                setTimeout(() => {
                    if (killedTracked.killed) return;
                    try {
                        instance.exports.timer_finished(id);
                    } catch(_error) {}
                }, ms)
            }
        },

        // Must create a new connection object. This implementation stores the created object in
        // `connections`.
        connection_new: (connectionId: number, addrPtr: number, addrLen: number, errorBufferIndexPtr: number) => {
            const instance = config.instance!;

            addrPtr >>>= 0;
            addrLen >>>= 0;
            errorBufferIndexPtr >>>= 0;

            if (!!connections[connectionId]) {
                throw new Error("internal error: connection already allocated");
            }

            try {
                if (killedTracked.killed)
                    throw new Error("killAll invoked");

                const address = buffer.utf8BytesToString(new Uint8Array(instance.exports.memory.buffer), addrPtr, addrLen);

                const connec = config.connect({
                    address,
                    onOpen: (info) => {
                        if (killedTracked.killed) return;
                        try {
                            switch (info.type) {
                                case 'single-stream': {
                                    instance.exports.connection_open_single_stream(connectionId, 0, info.initialWritableBytes, info.writeClosable ? 1 : 0);
                                    break
                                }
                                case 'multi-stream': {
                                    const handshakeTy = new Uint8Array(1 + info.localTlsCertificateMultihash.length + info.remoteTlsCertificateMultihash.length);
                                    buffer.writeUInt8(handshakeTy, 0, 0);
                                    handshakeTy.set(info.localTlsCertificateMultihash, 1)
                                    handshakeTy.set(info.remoteTlsCertificateMultihash, 1 + info.localTlsCertificateMultihash.length)
                                    config.bufferIndices[0] = handshakeTy;
                                    instance.exports.connection_open_multi_stream(connectionId, 0);
                                    delete config.bufferIndices[0]
                                    break
                                }
                            }
                        } catch(_error) {}
                    },
                    onConnectionReset: (message: string) => {
                        if (killedTracked.killed) return;
                        try {
                            config.bufferIndices[0] = new TextEncoder().encode(message);
                            instance.exports.connection_reset(connectionId, 0);
                            delete config.bufferIndices[0]
                        } catch(_error) {}
                    },
                    onWritableBytes: (numExtra, streamId) => {
                        if (killedTracked.killed) return;
                        try {
                            instance.exports.stream_writable_bytes(
                                connectionId,
                                streamId || 0,
                                numExtra,
                            );
                        } catch(_error) {}
                    },
                    onMessage: (message: Uint8Array, streamId?: number) => {
                        if (killedTracked.killed) return;
                        try {
                            config.bufferIndices[0] = message;
                            instance.exports.stream_message(connectionId, streamId || 0, 0);
                            delete config.bufferIndices[0]
                        } catch(_error) {}
                    },
                    onStreamOpened: (streamId: number, direction: 'inbound' | 'outbound', initialWritableBytes) => {
                        if (killedTracked.killed) return;
                        try {
                            instance.exports.connection_stream_opened(
                                connectionId,
                                streamId,
                                direction === 'outbound' ? 1 : 0,
                                initialWritableBytes
                            );
                        } catch(_error) {}
                    },
                    onStreamReset: (streamId: number) => {
                        if (killedTracked.killed) return;
                        try {
                            instance.exports.stream_reset(connectionId, streamId);
                        } catch(_error) {}
                    }
                
                });

                connections[connectionId] = connec;
                return 0;

            } catch (error) {
                const isBadAddress = error instanceof ConnectionError;
                let errorStr = "Unknown error";
                if (error instanceof Error) {
                    errorStr = error.toString();
                }

                const mem = new Uint8Array(instance.exports.memory.buffer);
                config.bufferIndices[0] = new TextEncoder().encode(errorStr)
                buffer.writeUInt32LE(mem, errorBufferIndexPtr, 0);
                buffer.writeUInt8(mem, errorBufferIndexPtr + 4, isBadAddress ? 1 : 0);
                return 1;
            }
        },

        // Must close and destroy the connection object.
        reset_connection: (connectionId: number) => {
            if (killedTracked.killed) return;
            const connection = connections[connectionId]!;
            connection.reset();
            delete connections[connectionId];
        },

        // Opens a new substream on a multi-stream connection.
        connection_stream_open: (connectionId: number) => {
            const connection = connections[connectionId]!;
            connection.openOutSubstream()
        },

        // Closes a substream on a multi-stream connection.
        connection_stream_reset: (connectionId: number, streamId: number) => {
            const connection = connections[connectionId]!;
            connection.reset(streamId)
        },

        // Must queue the data found in the WebAssembly memory at the given pointer. It is assumed
        // that this function is called only when the connection is in an open state.
        stream_send: (connectionId: number, streamId: number, ptr: number, len: number) => {
            if (killedTracked.killed) return;
    
            const instance = config.instance!;

            ptr >>>= 0;
            len >>>= 0;

            const data = new Uint8Array(instance.exports.memory.buffer).slice(ptr, ptr + len);
            const connection = connections[connectionId]!;
            connection.send(data, streamId);  // TODO: docs says the streamId is provided only for multi-stream connections, but here it's always provided
        },

        stream_send_close: (connectionId: number, streamId: number) => {
            if (killedTracked.killed) return;
    
            const connection = connections[connectionId]!;
            connection.closeSend(streamId);  // TODO: docs says the streamId is provided only for multi-stream connections, but here it's always provided
        },

        current_task_entered: (ptr: number, len: number) => {
            if (killedTracked.killed) return;

            const instance = config.instance!;

            ptr >>>= 0;
            len >>>= 0;

            const taskName = buffer.utf8BytesToString(new Uint8Array(instance.exports.memory.buffer), ptr, len);
            if (config.currentTaskCallback)
                config.currentTaskCallback(taskName);
        },

        current_task_exit: () => {
            if (killedTracked.killed) return;
            if (config.currentTaskCallback)
                config.currentTaskCallback(null);
        }
    };

    return { imports, killAll }
}
