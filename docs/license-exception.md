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

# Static Linking Exception for the C library (Draft)

Added on 2026-11-25 for M10 (`m10-satellite.md`). Same status as above:
**draft, not yet in force and not yet legally reviewed.**

## Purpose

The microcontroller library under `firmware/` is under LGPL-3.0-or-later,
so that firmware using it need not be under the GPL. Firmware is almost
always linked statically into one image. Plain LGPL would then require
whoever ships a device to let users relink it with a changed library
(object files or source of the whole firmware). That burden fits a
desktop library, not a button with a light. The exception lifts it for
the library and keeps the rest of the LGPL: changes to the library itself
stay under the LGPL and must be published with the device.

It applies only to the code under `firmware/`. App, core and clients stay
under GPL-3.0-or-later with the app store exception, the relay under
AGPL-3.0-or-later.

## Proposed wording

> **Additional permission under GNU LGPL version 3 section 7**
>
> As a special exception, the copyright holders of this Library give you
> permission to combine this Library, in unmodified or modified form, with
> independent modules into a single executable, including by static
> linking, and to convey the resulting executable under terms of your
> choice, without the obligations of section 4 d) and e) of the GNU Lesser
> General Public License to provide the means to recombine or relink the
> executable with a modified version of the Library.
>
> This permission does not apply to the Library itself: if you convey the
> executable, you must also convey, or offer as section 6 of the GNU General
> Public License requires, the Corresponding Source of the version of the
> Library it contains, including your modifications, under this License.
> An independent module is a module that is not derived from or based on
> this Library. If you modify this Library, you may extend this exception
> to your version of the Library, but you are not obliged to do so.

## What is still missing

1. Legal review, together with the app store exception.
2. After that: a `LICENSE` (LGPL-3.0) and `LICENSE-EXCEPTION` in `firmware/`
   and in the mirrored repository, and SPDX headers
   `LGPL-3.0-or-later WITH <exception>` in its sources.
