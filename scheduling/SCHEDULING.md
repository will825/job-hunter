# Daily digest scheduling (macOS)

Make Job Hunter scan and email you every morning automatically. Your Mac needs
to be **on and logged in** at the scheduled time — the screen can be off/asleep.

## 1. Set up email (one time) — via Resend (free)
In the app's Email panel (or `profile.toml` `[email]`), enable it and set your
`to` address. Leave `from` as `onboarding@resend.dev` to email yourself with no
domain setup. Then create a free API key at https://resend.com and put it in
`.env`:
```
RESEND_API_KEY=your_resend_key
```
Test it once by hand:
```bash
cargo run -- digest
```
The first run establishes a baseline (no email). Run it again and you'll get a
real email if there are new matches.

## 2. Build the release binary (faster than `cargo run`)
```bash
cargo build --release
```

## 3. Install the schedule
```bash
cp "scheduling/com.jobhunter.daily.plist" ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.jobhunter.daily.plist
```
Run it immediately to test:
```bash
launchctl start com.jobhunter.daily
tail -f digest.log
```

## 4. (Optional) Wake the Mac so it runs even from sleep
```bash
sudo pmset repeat wake MTWRFSU 06:58:00
```
This wakes the Mac at 6:58 AM daily; the digest runs at 7:00.

## Change the time
Edit `Hour`/`Minute` in the plist, then reload:
```bash
launchctl unload ~/Library/LaunchAgents/com.jobhunter.daily.plist
cp "scheduling/com.jobhunter.daily.plist" ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.jobhunter.daily.plist
```

## Turn it off
```bash
launchctl unload ~/Library/LaunchAgents/com.jobhunter.daily.plist
rm ~/Library/LaunchAgents/com.jobhunter.daily.plist
```
(To also stop the wake schedule: `sudo pmset repeat cancel`.)
