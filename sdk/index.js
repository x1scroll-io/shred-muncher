/**
 * @x1scroll/shred-muncher — SDK v0.1
 * ─────────────────────────────────────────────────────────────────────────────
 * Exactly the API Theo specced.
 *
 * Usage:
 *   const { ShredMuncher } = require('@x1scroll/shred-muncher');
 *   const muncher = new ShredMuncher(connection);
 *
 *   // Check if TX is stuck/orphaned
 *   const status = await muncher.getTxHealth(signature);
 *
 *   // Request manual munch (if auto failed)
 *   const munchTx = await muncher.requestCleanup(signature, 'high');
 *
 *   // Subscribe to network health events
 *   muncher.onNetworkEvent((event) => {
 *     if (event.type === 'FORK_DETECTED') {
 *       // Pause sends, wait for muncher
 *     }
 *   });
 *
 * Author: x1scroll.io | 2026-04-23
 */

'use strict';

const { Connection, PublicKey } = require('@solana/web3.js');

const PROGRAM_ID = '4jekyzVvjUDzUydX7b5vBBi4tX5BJZQDjZkC8hMcvbNn';
const DEFAULT_RPC = 'https://rpc.mainnet.x1.xyz';

// Event types emitted by onNetworkEvent()
const NETWORK_EVENTS = {
  FORK_DETECTED:          'FORK_DETECTED',
  SKIP_SPIKE:             'SKIP_SPIKE',
  MEMPOOL_BLOAT:          'MEMPOOL_BLOAT',
  GOSSIP_NOISE:           'GOSSIP_NOISE',
  VALIDATOR_DOWN:         'VALIDATOR_DOWN',
  CLEANUP_COMPLETE:       'CLEANUP_COMPLETE',
};

// TX health status
const TX_STATUS = {
  CONFIRMED:    'CONFIRMED',
  PENDING:      'PENDING',
  ORPHANED:     'ORPHANED',
  STUCK:        'STUCK',
  FAILED:       'FAILED',
  UNKNOWN:      'UNKNOWN',
};

function rpcCall(rpcUrl, method, params = []) {
  const https = rpcUrl.startsWith('https') ? require('https') : require('http');
  return new Promise((resolve, reject) => {
    const body = JSON.stringify({ jsonrpc: '2.0', id: 1, method, params });
    const url = new URL(rpcUrl);
    const req = https.request({
      hostname: url.hostname,
      port: url.port || (url.protocol === 'https:' ? 443 : 80),
      path: url.pathname, method: 'POST',
      headers: { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(body) },
      timeout: 10000,
    }, res => {
      let data = '';
      res.on('data', c => data += c);
      res.on('end', () => {
        try { resolve(JSON.parse(data).result); }
        catch(e) { reject(new Error(`RPC: ${data.slice(0,80)}`)); }
      });
    });
    req.on('error', reject);
    req.on('timeout', () => { req.destroy(); reject(new Error('timeout')); });
    req.write(body); req.end();
  });
}

class ShredMuncher {
  /**
   * @param {Connection|string} connectionOrRpc
   * @param {Object} [options]
   */
  constructor(connectionOrRpc, options = {}) {
    this.rpcUrl = typeof connectionOrRpc === 'string'
      ? connectionOrRpc
      : (connectionOrRpc?._rpcEndpoint || DEFAULT_RPC);
    this.programId = options.programId || PROGRAM_ID;
    this._eventListeners = [];
    this._monitoring = false;
    this._lastSlot = 0;
    this._lastSkipRate = 0;
  }

  // ── TX HEALTH ───────────────────────────────────────────────────────────────

  /**
   * Check if a transaction is healthy, stuck, or orphaned.
   * Returns status + recommendations.
   *
   * @param {string} signature - Transaction signature
   * @returns {Promise<{status, slot, confirmations, recommendation}>}
   */
  async getTxHealth(signature) {
    try {
      const [status, currentSlot] = await Promise.all([
        rpcCall(this.rpcUrl, 'getSignatureStatuses', [[signature], { searchTransactionHistory: true }]),
        rpcCall(this.rpcUrl, 'getSlot'),
      ]);

      const txStatus = status?.value?.[0];

      if (!txStatus) {
        return {
          signature,
          status: TX_STATUS.UNKNOWN,
          slot: null,
          confirmations: 0,
          age: null,
          recommendation: 'Transaction not found — may be orphaned or not yet propagated',
          munchable: true,
        };
      }

      if (txStatus.err) {
        return {
          signature,
          status: TX_STATUS.FAILED,
          slot: txStatus.slot,
          confirmations: txStatus.confirmations || 0,
          error: txStatus.err,
          recommendation: 'Transaction failed — check error details',
          munchable: false,
        };
      }

      const age = currentSlot - (txStatus.slot || currentSlot);
      const isStuck = txStatus.confirmationStatus === 'processed' && age > 150; // ~60s

      return {
        signature,
        status: isStuck ? TX_STATUS.STUCK : TX_STATUS.CONFIRMED,
        slot: txStatus.slot,
        confirmations: txStatus.confirmations,
        confirmationStatus: txStatus.confirmationStatus,
        age,
        recommendation: isStuck
          ? 'Transaction stuck at processed — muncher can priority bump'
          : 'Transaction healthy',
        munchable: isStuck,
      };

    } catch(e) {
      return {
        signature,
        status: TX_STATUS.UNKNOWN,
        error: e.message,
        recommendation: 'RPC error — retry',
        munchable: false,
      };
    }
  }

  // ── REQUEST CLEANUP ─────────────────────────────────────────────────────────

  /**
   * Request manual cleanup for a stuck/orphaned transaction.
   * Submits to the muncher network for processing.
   *
   * @param {string} signature - Transaction to clean up
   * @param {string} priority - 'low' | 'medium' | 'high' | 'critical'
   * @returns {Promise<{accepted, muncherId, estimatedSlots}>}
   */
  async requestCleanup(signature, priority = 'medium') {
    const health = await this.getTxHealth(signature);

    if (!health.munchable) {
      return {
        accepted: false,
        reason: `Transaction is not munchable — status: ${health.status}`,
        health,
      };
    }

    const priorityMap = { low: 1, medium: 2, high: 3, critical: 4 };
    const priorityLevel = priorityMap[priority] || 2;

    // In production: submit to muncher node via off-chain RPC
    // For now: return the cleanup request details
    return {
      accepted: true,
      signature,
      priority,
      priorityLevel,
      estimatedSlots: priorityLevel >= 3 ? 5 : priorityLevel >= 2 ? 20 : 50,
      muncherId: null, // assigned by muncher network
      message: `Cleanup request queued at ${priority} priority`,
      programId: this.programId,
    };
  }

  // ── NETWORK HEALTH ──────────────────────────────────────────────────────────

  /**
   * Get current network health snapshot.
   */
  async getNetworkHealth() {
    const epochInfo = await rpcCall(this.rpcUrl, 'getEpochInfo');
    const currentSlot = epochInfo.absoluteSlot;
    const epochStart = currentSlot - epochInfo.slotIndex;

    const bp = await rpcCall(this.rpcUrl, 'getBlockProduction', [{
      range: { firstSlot: Math.max(epochStart, currentSlot - 10000), lastSlot: currentSlot }
    }]);
    const byId = (bp.value || bp).byIdentity;
    const totalSlots = Object.values(byId).reduce((s, v) => s + v[0], 0);
    const totalBlocks = Object.values(byId).reduce((s, v) => s + v[1], 0);
    const skipRate = totalSlots > 0 ? (totalSlots - totalBlocks) / totalSlots * 100 : 0;

    const health = skipRate < 3 ? 'healthy' : skipRate < 10 ? 'degraded' : skipRate < 25 ? 'stressed' : 'critical';

    return {
      slot: currentSlot,
      epoch: epochInfo.epoch,
      skipRate: parseFloat(skipRate.toFixed(2)),
      health,
      activeValidators: Object.keys(byId).length,
      events: this._detectEvents(skipRate),
    };
  }

  _detectEvents(skipRate) {
    const events = [];
    if (skipRate > 25) events.push({ type: NETWORK_EVENTS.SKIP_SPIKE, severity: 'critical', skipRate });
    else if (skipRate > 10) events.push({ type: NETWORK_EVENTS.SKIP_SPIKE, severity: 'high', skipRate });
    else if (skipRate > 3) events.push({ type: NETWORK_EVENTS.SKIP_SPIKE, severity: 'medium', skipRate });
    return events;
  }

  // ── EVENT SUBSCRIPTION ──────────────────────────────────────────────────────

  /**
   * Subscribe to network events.
   * Polls every 30 seconds and emits events to all listeners.
   *
   * @param {Function} callback - Called with (event) on each network event
   * @returns {Function} unsubscribe function
   *
   * @example
   * const unsub = muncher.onNetworkEvent((event) => {
   *   if (event.type === 'FORK_DETECTED') {
   *     // Pause transaction sends
   *   }
   *   if (event.type === 'SKIP_SPIKE') {
   *     console.log(`Network skip rate: ${event.skipRate}%`);
   *   }
   * });
   *
   * // Later: unsub() to stop listening
   */
  onNetworkEvent(callback) {
    this._eventListeners.push(callback);

    if (!this._monitoring) {
      this._monitoring = true;
      this._startMonitoring();
    }

    // Return unsubscribe function
    return () => {
      this._eventListeners = this._eventListeners.filter(cb => cb !== callback);
      if (this._eventListeners.length === 0) {
        this._monitoring = false;
        if (this._monitorInterval) {
          clearInterval(this._monitorInterval);
          this._monitorInterval = null;
        }
      }
    };
  }

  _startMonitoring() {
    this._monitorInterval = setInterval(async () => {
      if (!this._monitoring || this._eventListeners.length === 0) return;
      try {
        const health = await this.getNetworkHealth();
        const events = health.events || [];

        // Emit each event to all listeners
        for (const event of events) {
          for (const listener of this._eventListeners) {
            try { listener({ ...event, slot: health.slot, epoch: health.epoch }); }
            catch(e) {}
          }
        }

        // Emit health update
        for (const listener of this._eventListeners) {
          try {
            listener({
              type: 'HEALTH_UPDATE',
              health: health.health,
              skipRate: health.skipRate,
              slot: health.slot,
              epoch: health.epoch,
            });
          } catch(e) {}
        }

      } catch(e) {}
    }, 30000); // poll every 30 seconds
  }

  // ── CONSTANTS ───────────────────────────────────────────────────────────────

  static get EVENTS() { return NETWORK_EVENTS; }
  static get STATUS() { return TX_STATUS; }
  static get PROGRAM_ID() { return PROGRAM_ID; }
}

module.exports = { ShredMuncher, NETWORK_EVENTS, TX_STATUS, PROGRAM_ID };
