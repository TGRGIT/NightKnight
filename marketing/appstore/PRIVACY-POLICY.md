# NightKnight — Privacy Policy

**Last updated: 28 June 2026**

> Fill in before publishing: replace `[Developer name]`, `[contact email]`, and
> `[privacy policy URL]` with your details, host this page at a public HTTPS URL, and
> paste that URL into App Store Connect (App Privacy → Privacy Policy URL). Review the
> wording with your own advisor — this is a template, not legal advice.

This Privacy Policy explains how the **NightKnight** iOS app ("the app", "NightKnight")
handles your information. It is published by **[Developer name]** ("we", "us").

## The short version

- NightKnight is a **client app for a server you run or choose**. It only sends your data
  to the NightKnight server *you* configure (for example `https://nightknight.cooney.be`).
- We (the app's developer) **do not operate a server for the app, do not receive your
  data, and do not have access to it.**
- There is **no analytics, no advertising, no tracking, and no third‑party SDKs.** We do
  not sell, rent, or share your data with anyone.
- Your glucose data, settings, and credentials are stored **on your device** (and, if you
  choose, in **Apple Health** and on **your server**).

## How NightKnight works

NightKnight displays continuous glucose monitoring (CGM) data and statistics. It does this
by connecting to a **NightKnight backend that you (or your provider) operate** — it is not
a service we host. You point the app at your server's address and authenticate with a
device token you create in your server's web interface.

Because the backend is yours, **you (or whoever runs your deployment) are the controller of
the data held on that server.** This policy covers what the *app on your device* does; how
your server stores and processes data is governed by that deployment's own terms.

## Information the app handles

NightKnight only handles the data needed to show your readings and to do what you ask:

- **Glucose and analytics data.** The app requests your current reading, recent readings,
  and computed statistics (e.g. time‑in‑range, GMI/eA1c estimates, AGP) from your server,
  and displays them. This data is health‑related and is treated as sensitive.
- **Apple Health (HealthKit), only with your permission.** If you enable it, the app can:
  - **write** blood‑glucose readings it received from your server into Apple Health; and
  - **read** blood‑glucose from Apple Health to display it (for example when Apple Health
    is your data source), including waking the app when new glucose is written to Health.
  Health data stays on your device and in Apple Health. **The app does not upload your
  Apple Health data to your server or anywhere else, and never uses Health data for
  advertising or shares it with any third party.** You can review or revoke Health access
  at any time in **Settings → Health → Data Access & Devices → NightKnight**.
- **Connection settings and credentials.** Your server URL, your device token
  (`api-secret`), and — if your server is behind Cloudflare Access — an optional Access
  service‑token ID and secret. These are stored locally on your device and sent to your
  server (and only your server) to authenticate your requests.
- **Time zone.** Your device's current UTC offset (in minutes) is sent to your server so
  time‑of‑day statistics (such as overnight patterns and AGP) are localised. No precise
  location is collected or used.
- **Push notification token.** If your server supports background refresh, the app
  registers your device's Apple Push Notification service (APNs) token and its environment
  (sandbox/production) with **your server**, so your server can send silent pushes that
  wake the app to fetch new readings.
- **Preferences.** Display unit (mg/dL or mmol/L), trailing period, alarm on/off and
  thresholds, and Apple Health toggles — stored locally on your device.

We do **not** collect names, email addresses, contacts, advertising identifiers, precise
location, device fingerprints, usage analytics, or crash/diagnostic reports.

## Where your data is stored

- **On your device.** Settings, credentials (server URL, device token, optional Cloudflare
  Access service‑token ID/secret), and a cached copy of your most recent reading are stored
  in the app's shared App Group container (`group.be.cooney.nightknight`) so the app, its
  Home Screen widget, and the Apple Watch app can display them. This container is sandboxed
  to NightKnight and is **included in your encrypted device backups** (iCloud or Finder) if
  you back up your device. Configuration is also shared from your iPhone to your paired
  Apple Watch over Apple's on‑device WatchConnectivity.
- **In Apple Health**, if you enable HealthKit — managed by you in the Health app.
- **On your server**, which you control.

We do not store any of your data on infrastructure we operate, because we do not operate
any.

## When your data leaves the device, and to whom

Your data is only transmitted to:

1. **Your NightKnight server** — every API request goes to the address you configured, over
   HTTPS, carrying your device token (and, if configured, your Cloudflare Access service
   token). The app also sends your time‑zone offset and your APNs push token to that server.
2. **Apple** — to deliver silent and local notifications, NightKnight relies on Apple's Push
   Notification service and the iOS notification system, and on Apple Health if you enable
   it. Apple's handling of this data is governed by Apple's Privacy Policy.

That's it. The app makes **no other network connections** — there are no analytics,
advertising, telemetry, or third‑party services of any kind.

## What we do not do

- We do **not** use any analytics, tracking, attribution, or advertising frameworks.
- We do **not** include any third‑party SDKs.
- We do **not** sell, rent, trade, or share your personal or health data with anyone.
- We do **not** use your data for advertising or marketing.
- We do **not** create an account with us or profile you — you authenticate only to your
  own server.

## Notifications

If you turn on alarms, NightKnight evaluates your readings **on your device** and shows
local notifications for low, high, or rapidly‑dropping glucose. Notification content is
generated locally and is not sent to us. Silent (background) push notifications come from
**your server** via Apple's APNs and contain no readable glucose content — they simply wake
the app to refresh.

## Data retention and deletion

- Data on your device persists until you delete it or uninstall the app. **Deleting the app
  removes its locally stored settings, credentials, and cached reading** from the device.
  (Data in your encrypted device backups follows your backup settings.)
- **Apple Health** data is retained and removed by you in the Health app; revoking
  NightKnight's Health access stops further reads/writes.
- Readings stored on **your server** are retained and deleted according to your deployment;
  revoke a device token there to immediately cut off the app's access.

## Security

- All communication with your server uses HTTPS (the app permits plain HTTP only for local
  development networks).
- Your server may sit behind Cloudflare Access; the app supports passing an Access service
  token so only authorised devices can reach it.
- Credentials are stored in the app's sandboxed App Group container. Note this container is
  included in device backups; protect your device with a passcode and use encrypted backups.
  Revoking a device token on your server disables a lost device's access.

## Children's privacy

NightKnight is not directed to children under 13 (or the equivalent minimum age in your
jurisdiction), and we do not knowingly collect data from children. The app is often used by
caregivers to follow a child's readings; in that case the data is supplied by the caregiver
and held on the caregiver's own server, not by us.

## Your rights and choices

Because your data lives on your device, in Apple Health, and on your own server, **you
control it directly**:

- Turn Apple Health access on/off in iOS Settings.
- Turn alarms and notifications on/off in the app and in iOS Settings.
- Remove credentials by clearing them in Settings or deleting the app.
- Access, export, or delete the readings held on your server using your server's own tools,
  and revoke device tokens there.

Depending on where you live, you may have rights under laws such as the EU/UK GDPR or the
CCPA/CPRA (to access, correct, delete, or port your data, and to object to processing).
Since we do not hold your data, direct such requests to the operator of your NightKnight
server; for questions about the app itself, contact us below.

## International users

The app runs entirely on your device and communicates only with the server you choose, so
any cross‑border transfer of your data is determined by where *your* server is hosted, not
by us.

## Not a medical device

NightKnight displays data for information only and is **not a medical device**. Do not use
it to make treatment decisions; always confirm with your CGM/receiver and your healthcare
provider. This section is provided for clarity and does not form part of our data practices.

## Changes to this policy

If we change how the app handles data, we will update this policy and its "Last updated"
date, and (for material changes) note it in the app's release notes.

## Contact

Questions about this policy or the app:

- **[Developer name]**
- Email: **[contact email]**
- Policy URL: **[privacy policy URL]**
