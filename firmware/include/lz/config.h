/* SPDX-License-Identifier: LGPL-3.0-or-later WITH LicenseRef-Lebenszeichen-static-linking-exception */
/*
 * Compile-time limits of lz_core. Every array in the context struct is sized
 * from these, so the context has a fixed size and needs no allocator.
 *
 * Override any of them with -D on the compiler command line or by defining
 * them before including any lz header (for example in an SDK component's
 * build flags). Changing a limit changes the size of the context and the
 * maximum size of the persisted blob; blobs stay readable as long as every
 * stored count fits the new limits.
 */
#ifndef LZ_CONFIG_H
#define LZ_CONFIG_H

/* Contacts the satellite holds. */
#ifndef LZ_MAX_CONTACTS
#define LZ_MAX_CONTACTS 16
#endif

/* Relays the satellite keeps a connection to: the channel's and those of
 * the own mailboxes (decision 9 of m10-satellite.md). */
#ifndef LZ_MAX_RELAYS
#define LZ_MAX_RELAYS 3
#endif

/* Requests per contact and direction kept for the rules (last 7 days and
 * the open one). The satellite contact record carries at most 16. */
#ifndef LZ_MAX_REQUESTS
#define LZ_MAX_REQUESTS 16
#endif

/* Message identifiers remembered against replay, per contact. When full,
 * the oldest is forgotten and nothing sent at or before it is accepted
 * any more, so a replay is never let through. */
#ifndef LZ_MAX_SEEN
#define LZ_MAX_SEEN 24
#endif

/* The same for the channel, which carries more traffic. */
#ifndef LZ_MAX_CHANNEL_SEEN
#define LZ_MAX_CHANNEL_SEEN 64
#endif

/* Envelopes waiting to be delivered. */
#ifndef LZ_OUTBOX_SIZE
#define LZ_OUTBOX_SIZE 16
#endif

/* Down messages about contacts not known yet, kept for 24 hours. */
#ifndef LZ_MAX_PARKED
#define LZ_MAX_PARKED 8
#endif

/* Transfers of home_contact_part being put together at the same time. */
#ifndef LZ_MAX_TRANSFERS
#define LZ_MAX_TRANSFERS 2
#endif

/* Events waiting for lz_poll(); the oldest is dropped when full. */
#ifndef LZ_MAX_EVENTS
#define LZ_MAX_EVENTS 8
#endif

/* Relay addresses must start with https://. Define to 1 to also allow
 * http:// (development against a local relay only). */
#ifndef LZ_ALLOW_HTTP
#define LZ_ALLOW_HTTP 0
#endif

#if LZ_MAX_CONTACTS < 1 || LZ_MAX_CONTACTS > 250
#error "LZ_MAX_CONTACTS must be 1..250"
#endif
#if LZ_MAX_RELAYS < 1 || LZ_MAX_RELAYS > 8
#error "LZ_MAX_RELAYS must be 1..8"
#endif
#if LZ_MAX_REQUESTS < 2 || LZ_MAX_REQUESTS > 250
#error "LZ_MAX_REQUESTS must be 2..250"
#endif
#if LZ_MAX_SEEN < 4 || LZ_MAX_SEEN > 250 || LZ_MAX_CHANNEL_SEEN < 4 || LZ_MAX_CHANNEL_SEEN > 250
#error "LZ_MAX_SEEN and LZ_MAX_CHANNEL_SEEN must be 4..250"
#endif
#if LZ_OUTBOX_SIZE < 4 || LZ_OUTBOX_SIZE > 250
#error "LZ_OUTBOX_SIZE must be 4..250"
#endif
#if LZ_MAX_PARKED < 1 || LZ_MAX_PARKED > 250 || LZ_MAX_TRANSFERS < 1 || LZ_MAX_TRANSFERS > 16
#error "LZ_MAX_PARKED must be 1..250, LZ_MAX_TRANSFERS 1..16"
#endif
#if LZ_MAX_EVENTS < 2 || LZ_MAX_EVENTS > 64
#error "LZ_MAX_EVENTS must be 2..64"
#endif

#endif
