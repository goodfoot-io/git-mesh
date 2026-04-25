#!/bin/bash
# Paste-in script: configures the running container to ssh into
# devcontainer-git-mesh at 192.168.65.3:62409 as user 'node'.
set -euo pipefail

mkdir -p "$HOME/.ssh"
chmod 700 "$HOME/.ssh"

KEY_FILE="$HOME/.ssh/id_ed25519_devcontainer-git-mesh"
cat > "$KEY_FILE" <<'PRIVKEY'
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW
QyNTUxOQAAACDz9Bu1lJD/2EInNVUoKB8B5IhD3wudO2UnSIz9To6zTgAAAKCP3Nd5j9zX
eQAAAAtzc2gtZWQyNTUxOQAAACDz9Bu1lJD/2EInNVUoKB8B5IhD3wudO2UnSIz9To6zTg
AAAEApSNQWyJG8+9qK9XYt24tju3hZYSRJxOzZPAFCo7jUpPP0G7WUkP/YQic1VSgoHwHk
iEPfC507ZSdIjP1OjrNOAAAAGmRldmNvbnRhaW5lci1wZWVyLWdpdC1tZXNoAQID
-----END OPENSSH PRIVATE KEY-----
PRIVKEY
chmod 600 "$KEY_FILE"

# Pin the host key in the user's default known_hosts.
KNOWN="$HOME/.ssh/known_hosts"
touch "$KNOWN"
chmod 600 "$KNOWN"
HOST_LINE="[192.168.65.3]:62409 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIN/jL+A1S87kepzYfE2dql3aJGOGkTzxOnxu9U8IfM0+ devcontainer-host-git-mesh"
ssh-keygen -R "[192.168.65.3]:62409" -f "$KNOWN" >/dev/null 2>&1 || true
echo "$HOST_LINE" >> "$KNOWN"

# Add Host alias to ~/.ssh/config (replace any prior block for this alias).
CFG="$HOME/.ssh/config"
touch "$CFG"
chmod 600 "$CFG"
python3 - "$CFG" "devcontainer-git-mesh" "192.168.65.3" "62409" "$KEY_FILE" <<'PYEOF'
import sys, re
cfg_path, alias, host, port, keyfile = sys.argv[1:]
try:
    with open(cfg_path) as f:
        text = f.read()
except FileNotFoundError:
    text = ""
pattern = re.compile(
    r"(?ms)^Host\s+" + re.escape(alias) + r"\s*\n(?:[ \t].*\n?)*"
)
text = pattern.sub("", text)
if text and not text.endswith("\n"):
    text += "\n"
text += (
    f"Host {alias}\n"
    f"    HostName {host}\n"
    f"    Port {port}\n"
    f"    User node\n"
    f"    IdentityFile {keyfile}\n"
    f"    IdentitiesOnly yes\n"
    f"    StrictHostKeyChecking yes\n"
)
with open(cfg_path, "w") as f:
    f.write(text)
PYEOF

echo "Configured: ssh devcontainer-git-mesh  ->  node@192.168.65.3:62409"
