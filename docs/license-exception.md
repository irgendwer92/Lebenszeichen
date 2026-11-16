# App Store Exception (Draft)

Translated from the German draft on 2026-11-16. The legal review from M0 is still open; until then the wording is not final.

**Status: Draft. Not yet in force and not yet legally reviewed.**

## Purpose

Clients and core are under GPL-3.0-or-later. The terms of app
stores, above all Apple's App Store, impose restrictions on users that
are incompatible with the GPL. So that an iOS version remains possible, the
license should contain an additional permission under section 7 of the GPL
from the start. Anyone who contributes later does so under the same terms.

The relay is under AGPL-3.0-or-later and needs no exception.

## Proposed wording

> **Additional permission under GNU GPL version 3 section 7**
>
> If you convey this Program, or a work based on it, through an application
> store operated by a third party, and the terms of that store impose
> restrictions on recipients that would otherwise conflict with this License,
> you may convey the Program through that store under those terms, provided
> that the complete Corresponding Source of the conveyed version remains
> publicly available under this License without those additional
> restrictions.

## What is still missing

1. Legal review of the wording.
2. After that: copy the text into a file `LICENSE-EXCEPTION` in the root
   directory and refer to it in the license notice of the affected crates.
3. This must happen before the first outside contribution. As long as all code
   comes from one person, the license can still be freely adjusted.
