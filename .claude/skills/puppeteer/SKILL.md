---
name: puppeteer
description: Use puppeteer to connect to inspect VSCode 
---

<environment>
## Environment

```!
# === VSCode Extension Development Environment Check ===

CDP_URL="http://127.0.0.1:19222"
CDP_RESPONSE=$(curl -s --connect-timeout 2 "$CDP_URL/json/version" 2>/dev/null)
if [ -z "$CDP_RESPONSE" ]; then
  echo "❌ BLOCKED: VSCode not exposing CDP on port 19222"
  echo ""
  echo "⚠️ ALERT: Cannot connect to Chrome DevTools Protocol at ${CDP_URL}."
  echo "Launch VSCode with --remote-debugging-port=19222 to enable browser automation."
  echo "EDH commands, screenshot capture, and console log capture are unavailable."
  echo "Use the 'Debugging Without Puppeteer' techniques documented below instead."
  exit 1
fi

# Extract WS endpoint using node (more reliable than bash string manipulation)
WS_ENDPOINT=$(node -e "console.log(JSON.parse(process.argv[1]).webSocketDebuggerUrl)" "$CDP_RESPONSE" 2>/dev/null)
if [ -z "$WS_ENDPOINT" ]; then
  echo "❌ BLOCKED: Could not parse WebSocket endpoint from CDP response"
  echo ""
  echo "⚠️ ALERT: CDP responded but the WebSocket debugger URL is missing."
  echo "Use the 'Debugging Without Puppeteer' techniques documented below instead."
  exit 1
fi

node -e "require('puppeteer-core')" 2>/dev/null
if [ $? -ne 0 ]; then
  echo "❌ BLOCKED: puppeteer-core is not installed"
  echo ""
  echo "⚠️ ALERT: Install puppeteer-core in the workspace: yarn add -D puppeteer-core"
  echo "Use the 'Debugging Without Puppeteer' techniques documented below instead."
  exit 1
fi

echo "✓ Ready"
echo "WS_ENDPOINT=$WS_ENDPOINT"
```
</environment>

<puppeteer>

## Connection Template

**Important**: Run all scripts from aworkspace where `puppeteer-core` is installed in `node_modules`. Scripts run from `/tmp` or other directories will fail with module resolution errors.

```bash
node << 'EOF'
import puppeteer from "puppeteer-core";

const browser = await puppeteer.connect({
  browserWSEndpoint: "WS_ENDPOINT_FROM_ABOVE",
  defaultViewport: null  // Required: prevents Electron window resize
});

const pages = await browser.pages();
console.log("Connected, pages:", pages.length);

// ... your code here

await browser.disconnect();  // NOT close() - keeps VSCode running
EOF
```

### Screenshot Capture

```bash
node << 'EOF'
import puppeteer from "puppeteer-core";

const browser = await puppeteer.connect({
  browserWSEndpoint: "WS_ENDPOINT_FROM_ABOVE",
  defaultViewport: null
});

const pages = await browser.pages();
const page = pages[0];

// Full page - use captureBeyondViewport: false for Electron
await page.screenshot({
  path: "/tmp/current.png",
  captureBeyondViewport: false
});

// Element-specific
const element = await page.$(".my-component");
if (element) {
  await element.screenshot({ path: "/tmp/component.png" });
}

await browser.disconnect();
EOF
```

### Console Capture (Extension Logs)

```bash
node << 'EOF'
import puppeteer from "puppeteer-core";

const browser = await puppeteer.connect({
  browserWSEndpoint: "WS_ENDPOINT_FROM_ABOVE",
  defaultViewport: null
});

const page = (await browser.pages())[0];
const client = await page.createCDPSession();

await client.send("Runtime.enable");

const logs = [];
client.on("Runtime.consoleAPICalled", e => {
  const msg = e.args.map(a => a.value ?? a.description).join(" ");
  logs.push(`[${e.type}] ${msg}`);
  console.log(`[${e.type}]`, msg);
});

// Perform actions...
await new Promise(r => setTimeout(r, 5000));

console.log("Captured", logs.length, "log entries");

await client.detach();
await browser.disconnect();
EOF
```

</puppeteer>


