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

// This file launches a WebSocket server that exposes JSON-RPC functions.

import * as smoldot from '../dist/mjs/index-nodejs.js';
import { WebSocketServer } from 'ws';
import * as process from 'node:process';
import * as fs from 'node:fs';

// List of files containing chains available to the user.
// The first item has a specific role in that we always connect to it at initialization.
const chainSpecsFiles = [
    //'../../demo-chain-specs/westend.json',
    //'../../demo-chain-specs/westend-westmint.json',
    '../../demo-chain-specs/polkadot.json',
    '../../demo-chain-specs/astar.json',
    //'../../demo-chain-specs/polkadot-acala.json',
    //'../../demo-chain-specs/kusama.json',
    //'../../demo-chain-specs/kusama-statemine.json',
    //'../../demo-chain-specs/kusama-karura.json',
    //'../../demo-chain-specs/rococo.json',
    //'../../demo-chain-specs/rococo-canvas.json',
    '../../demo-chain-specs/tokyo-light.json',
    '../../demo-chain-specs/shibuya.json'
];

// Load all the files in a single map.
const chainSpecsById = {};
let firstChainSpecId = null;
for (const file of chainSpecsFiles) {
    const content = fs.readFileSync(file, 'utf8');
    const decoded = JSON.parse(content);
    if (!firstChainSpecId)
        firstChainSpecId = decoded.id;
    chainSpecsById[decoded.id] = {
        chainSpec: content,
        relayChain: decoded.relay_chain,
    };
}

const client = smoldot.start({
    maxLogLevel: 3,  // Can be increased for more verbosity
    forbidTcp: false,
    forbidWs: false,
    forbidNonLocalWs: false,
    forbidWss: false,
    cpuRateLimit: 0.5,
    logCallback: (_level, target, message) => {
        // As incredible as it seems, there is currently no better way to print the current time
        // formatted in a certain way.
        const now = new Date();
        const hours = ("0" + now.getHours()).slice(-2);
        const minutes = ("0" + now.getMinutes()).slice(-2);
        const seconds = ("0" + now.getSeconds()).slice(-2);
        const milliseconds = ("00" + now.getMilliseconds()).slice(-3);
        console.log(
            "[%s:%s:%s.%s] [%s] %s",
            hours, minutes, seconds, milliseconds, target, message
        );
    }
});

// Note that We call `addChain` again with the same chain spec again every time a new WebSocket
// connection is established, but smoldot will de-duplicate them and only connect to the chain
// once. By calling it now, we let smoldot start syncing that chain in the background even before
// a WebSocket connection has been established.
client
    .addChain({ chainSpec: chainSpecsById[firstChainSpecId].chainSpec })
    .catch((error) => {
        console.error("Error while adding chain: " + error);
        process.exit(1);
    });

// Start the WebSocket server listening on port 9944.
let wsServer = new WebSocketServer({
    port: 9944
});

console.log('JSON-RPC server now listening on port 9944');
console.log('Please visit one of:');
for (const chainId in chainSpecsById) {
    console.log('- ' + chainId + ': https://polkadot.js.org/apps/?rpc=ws%3A%2F%2F127.0.0.1%3A9944%2F' + chainId);
}
console.log('');

wsServer.on('connection', function (connection, request) {
    // Received a new incoming WebSocket connection.

    // Note that we don't care too much about sanitizing input as this is just a demo.
    const chainCfg = chainSpecsById[request.url.substring(1)];

    if (!chainCfg) {
        connection.close();
        return;
    }

    console.log('(demo) New JSON-RPC client connected: ' + request.socket.remoteAddress + '.');

    // Start loading the chain.
    let chain = (async () => {
        if (chainCfg.relayChain) {
            if (!chainSpecsById[chainCfg.relayChain])
                throw new Error("Couldn't find relay chain: " + chainCfg.relayChain);

            const relay = await client.addChain({
                chainSpec: chainSpecsById[chainCfg.relayChain].chainSpec,
                disableJsonRpc: true
            });

            const para = await client.addChain({
                chainSpec: chainCfg.chainSpec,
                potentialRelayChains: [relay]
            });

            (async () => {
                try {
                    while(true) {
                        const response = await para.nextJsonRpcResponse();
                        connection.send(response);
                    }
                } catch(_error) {}
            })()

            return { relay, para };
        } else {
            const relay = await client.addChain({
                chainSpec: chainCfg.chainSpec,
            });

            (async () => {
                try {
                    while(true) {
                        const response = await relay.nextJsonRpcResponse();
                        connection.send(response);
                    }
                } catch(_error) {}
            })()

            return {
                relay,
            };
        }
    })().catch((error) => {
        console.error("(demo) Error while adding chain: " + error);
        connection.close(1011); // Internal server error
    });

    // Receiving a message from the connection. This is a JSON-RPC request.
    connection.on('message', function (data, isBinary) {
        if (!isBinary) {
            const message = data.toString('utf8');
            chain
                .then(chain => {
                    if (chain.para)
                        chain.para.sendJsonRpc(message);
                    else
                        chain.relay.sendJsonRpc(message);
                })
                .catch((error) => {
                    console.error("(demo) Error during JSON-RPC request: " + error);
                    process.exit(1);
                });
        } else {
            connection.close(1002); // Protocol error
        }
    });

    // When the connection closes, remove the chains that have been added.
    connection.on('close', function (reasonCode, description) {
        console.log("(demo) JSON-RPC client " + request.socket.remoteAddress + ' disconnected.');
        chain.then(chain => {
            chain.relay.remove();
            if (chain.para)
                chain.para.remove();
        }).catch(() => { });
    });
});
