# Deploying on Linux / Raspberry Pi (always-on)

Run Job Hunter 24/7 on a small Linux box (e.g. a Raspberry Pi) so the dashboard
is always reachable and the digest goes out every morning — no Mac required.

This was tested on a **Raspberry Pi 3B (1 GB RAM), Raspberry Pi OS Lite 64-bit**.

## 1. Prerequisites

```bash
sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev git rsync
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

On a 1 GB Pi, add swap before building or the Rust build can run out of memory:

```bash
sudo fallocate -l 2G /swapfile && sudo chmod 600 /swapfile && sudo mkswap /swapfile && sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

## 2. Get the code + build

```bash
git clone https://github.com/YOUR_USER/job-hunter.git ~/JobHunter
cd ~/JobHunter
cp .env.example .env                 # then fill in your keys
cp profile.example.toml profile.toml # then make it yours
cargo build --release                # ~20–45 min on a Pi 3B (one time)
```

## 3. Run the web server as a service

```bash
sudo cp scheduling/linux/jobhunter.service /etc/systemd/system/
sudo sed -i "s/YOUR_USER/$USER/g" /etc/systemd/system/jobhunter.service
sudo systemctl daemon-reload
sudo systemctl enable --now jobhunter
systemctl status jobhunter          # expect: active (running)
```

The dashboard is now on `http://<host>:8787` and restarts on crash/reboot.

## 4. Schedule the 7 AM digest (cron)

```bash
crontab -e
```

Add (adjust the path to your checkout):

```
0 7 * * * cd /home/YOUR_USER/JobHunter && ./target/release/job_hunter digest >> /home/YOUR_USER/JobHunter/digest.log 2>&1
```

## 5. Reach it from anywhere (optional)

The app has **no authentication**, so don't expose it to the public internet.
[Tailscale](https://tailscale.com) puts the Pi on a private mesh network so you
can reach it securely from your phone/laptop anywhere:

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up
```

Then browse to `http://<pi-hostname>:8787` from any device on your tailnet.
