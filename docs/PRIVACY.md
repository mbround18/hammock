# Privacy Notice

This repository ships a self-hosted Discord captioning bot. You are solely responsible for
how, where, and for whom you run it. No hosted service is provided by the authors, and no
telemetry or analytics are collected on your behalf. Operating the bot in any environment
means you accept all legal and privacy obligations for the data it captures and stores.

## Data Flow Basics

- **Storage**: All transcriptions are written to the `captions/` directory (or the folder
  defined by `CAPTION_OUTPUT_DIR`). Files stay on the machine or volume you configure.
- **Third-Party Traffic**: The application does not send audio, text, or metadata to any
  external provider unless you explicitly supply an `OPENAPI_KEY`. When that key is set, the
  generated transcript text is submitted to OpenAI solely for summary generation. Remove the
  key or leave it unset to keep every byte local.
- **Self-Hosting**: This is designed for self-hosted deployments. If you expose the bot to
  other communities or organizations, you must disclose what data is captured, how long it is
  retained, and who can access it.

## Operational Responsibility

- You are the data controller. Ensure your deployment complies with Discord terms, local laws,
  and the privacy expectations of the people in each call.
- Provide clear notice in every server where you install the bot. Users should know that
  joining a voice channel with the bot present results in recording/transcription.
- Back up or delete caption files according to your own retention and disclosure policies.

## Speaker Identification Modes

The bot adapts to how Discord voice encryption works:

1. **Transparent Mode (bot first)**: If the bot joins a channel before anyone speaks,
   Discord exposes canonical user metadata. Captions use each participant's Discord username
   or nickname.
2. **Randomized Mode (bot joins later)**: If the bot arrives after a call is underway,
   Discord's end-to-end encryption intentionally hides exact mappings. In these cases every
   speaker receives a stable random number label for the duration of that session
   (e.g., Speaker 1, Speaker 2). This safeguard is intrinsic to Discord and will not be
   changed.

## Summary

- The software stores voice-derived text locally and sends it nowhere else unless you add an
  OpenAI API key for optional summaries.
- No warranties, hosting, or compliance guarantees are offered; you run it, you own it.
- Inform users, protect their data, and review the generated transcripts regularly.

If you cannot accept full responsibility for the content captured by this bot, do not deploy it.
