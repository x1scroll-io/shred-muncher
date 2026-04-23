#!/bin/bash
# ─────────────────────────────────────────────────────────────────────────────
# x1scroll Shred Muncher — One-Line Installer
# Usage: curl -sSL https://raw.githubusercontent.com/x1scroll-io/shred-muncher/main/install.sh | bash
# ─────────────────────────────────────────────────────────────────────────────

set -e

MUNCHER_DIR="$HOME/shred-muncher"
REPO="https://raw.githubusercontent.com/x1scroll-io/shred-muncher/main"
RPC_URL="https://rpc.mainnet.x1.xyz"
PROGRAM_ID="4jekyzVvjUDzUydX7b5vBBi4tX5BJZQDjZkC8hMcvbNn"
MIN_BOND=500  # XNT

echo ""
echo "🦷 x1scroll Shred Muncher — Node Installer"
echo "────────────────────────────────────────────"
echo ""

# ── 1. Check/install Node.js ──────────────────────────────────────────────────
if ! command -v node &>/dev/null || [ "$(node -e 'process.exit(parseInt(process.version.slice(1)) < 18 ? 1 : 0)' 2>/dev/null; echo $?)" = "1" ]; then
  echo "📦 Installing Node.js v20..."
  export NVM_DIR="$HOME/.nvm"
  curl -fsSL https://raw.githubusercontent.com/nvm-sh/nvm/v0.39.0/install.sh | bash &>/dev/null
  source "$NVM_DIR/nvm.sh" &>/dev/null
  nvm install 20 &>/dev/null
  source "$NVM_DIR/nvm.sh" &>/dev/null
  echo "✅ Node.js $(node --version) installed"
else
  echo "✅ Node.js $(node --version) found"
fi

# ── 2. Install PM2 ────────────────────────────────────────────────────────────
if ! command -v pm2 &>/dev/null; then
  echo "📦 Installing PM2..."
  npm install -g pm2 &>/dev/null
  echo "✅ PM2 installed"
else
  echo "✅ PM2 found"
fi

# ── 3. Create muncher directory ───────────────────────────────────────────────
mkdir -p "$MUNCHER_DIR"
cd "$MUNCHER_DIR"

# ── 4. Download muncher files ─────────────────────────────────────────────────
echo "📥 Downloading Shred Muncher..."
curl -sSL "$REPO/muncher-daemon.js" -o muncher-daemon.js 2>/dev/null || \
  curl -sSL "https://raw.githubusercontent.com/x1scroll-io/shred-muncher/main/muncher-daemon.js" -o muncher-daemon.js
cat > package.json << 'PKGEOF'
{"name":"shred-muncher-node","version":"1.0.0","dependencies":{"@solana/web3.js":"^1.98.0"}}
PKGEOF
npm install &>/dev/null
echo "✅ Muncher downloaded"

# ── 5. Detect region ──────────────────────────────────────────────────────────
echo ""
echo "🌍 Detecting your region..."
REGION="us"
if curl -s --max-time 3 https://ipinfo.io/country 2>/dev/null | grep -qE "GB|DE|FR|NL|SE|NO|FI|PL|IT|ES|PT|CH|AT|BE|IE"; then
  REGION="eu"
elif curl -s --max-time 3 https://ipinfo.io/country 2>/dev/null | grep -qE "SG|JP|KR|CN|AU|NZ|IN|TH|ID|MY|PH"; then
  REGION="apac"
fi
echo "✅ Region: $REGION"

# ── 6. Generate bond wallet ───────────────────────────────────────────────────
echo ""
echo "🔑 Generating your Muncher bond wallet..."
node -e "
const { Keypair } = require('./node_modules/@solana/web3.js');
const fs = require('fs');

const kp = Keypair.generate();
const secret = Array.from(kp.secretKey);
fs.writeFileSync('./muncher-wallet.json', JSON.stringify(secret));

// base58 encode
const alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
let digits = [0];
for (let i = 0; i < kp.secretKey.length; i++) {
  let carry = kp.secretKey[i];
  for (let j = 0; j < digits.length; j++) { carry += digits[j] << 8; digits[j] = carry % 58; carry = (carry / 58) | 0; }
  while (carry > 0) { digits.push(carry % 58); carry = (carry / 58) | 0; }
}
const privB58 = digits.reverse().map(d => alphabet[d]).join('');

console.log('');
console.log('════════════════════════════════════════════════════');
console.log('  SAVE YOUR BOND WALLET — REQUIRED TO EXIT');
console.log('════════════════════════════════════════════════════');
console.log('  Public key:  ' + kp.publicKey.toBase58());
console.log('  Private key: ' + privB58);
console.log('════════════════════════════════════════════════════');
console.log('');
console.log('  Send exactly 500+ XNT to this address to activate');
console.log('  your Shred Muncher node.');
console.log('');
console.log('  Bond is returned when you exit the network.');
console.log('  Slashed 10% if you submit bad cleanup actions.');
console.log('');
"

MUNCHER_PUBKEY=$(node -e "
const { Keypair } = require('./node_modules/@solana/web3.js');
const fs = require('fs');
const kp = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync('./muncher-wallet.json'))));
console.log(kp.publicKey.toBase58());
" 2>/dev/null)

# ── 7. Write config ───────────────────────────────────────────────────────────
cat > muncher-config.json << CONFIGEOF
{
  "rpcUrl": "https://rpc.mainnet.x1.xyz",
  "programId": "$PROGRAM_ID",
  "bondWalletPath": "$MUNCHER_DIR/muncher-wallet.json",
  "region": "$REGION",
  "minBondXNT": $MIN_BOND,
  "scanIntervalMs": 5000,
  "logCleanups": true,
  "telegramBotToken": "",
  "telegramChatId": ""
}
CONFIGEOF

echo "✅ Config written"

# ── 8. Wait for funding ───────────────────────────────────────────────────────
echo ""
echo "════════════════════════════════════════════════════"
echo "  ACTION REQUIRED"
echo "════════════════════════════════════════════════════"
echo "  Send 500+ XNT to fund your bond wallet:"
echo ""
echo "  $MUNCHER_PUBKEY"
echo ""
echo "  Once funded, run: cd $MUNCHER_DIR && pm2 start muncher-daemon.js --name shred-muncher"
echo "════════════════════════════════════════════════════"
echo ""

# ── 9. Launch daemon ──────────────────────────────────────────────────────────
echo "Starting Shred Muncher daemon (will activate when bond is funded)..."
pm2 start muncher-daemon.js --name shred-muncher 2>/dev/null || true
pm2 save &>/dev/null

echo ""
echo "════════════════════════════════════════════════════"
echo "  🦷 Shred Muncher installed!"
echo "════════════════════════════════════════════════════"
echo ""
echo "  Fund your bond wallet with 500+ XNT to go live."
echo ""
echo "  Commands:"
echo "    pm2 logs shred-muncher      # view live cleanup logs"
echo "    pm2 status shred-muncher    # check status"
echo "    pm2 restart shred-muncher   # restart"
echo ""
echo "  Docs: github.com/x1scroll-io/shred-muncher"
echo "  Support: @ArnettX1 on Telegram"
echo ""
