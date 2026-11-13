# App-Store-Ausnahme (Entwurf)

**Status: Entwurf. Noch nicht in Kraft und noch nicht juristisch geprüft.**

## Wozu

Clients und Kern stehen unter GPL-3.0-or-later. Die Bedingungen von App
Stores, vor allem von Apples App Store, legen Nutzern Beschränkungen auf, die
sich mit der GPL nicht vertragen. Damit eine iOS-Version möglich bleibt, soll
die Lizenz von Anfang an eine Zusatzerlaubnis nach Abschnitt 7 der GPL
enthalten. Wer später beiträgt, tut das dann unter denselben Bedingungen.

Das Relay steht unter AGPL-3.0-or-later und braucht keine Ausnahme.

## Vorgeschlagener Wortlaut

> **Additional permission under GNU GPL version 3 section 7**
>
> If you convey this Program, or a work based on it, through an application
> store operated by a third party, and the terms of that store impose
> restrictions on recipients that would otherwise conflict with this License,
> you may convey the Program through that store under those terms, provided
> that the complete Corresponding Source of the conveyed version remains
> publicly available under this License without those additional
> restrictions.

## Was noch fehlt

1. Juristisches Gegenlesen des Wortlauts.
2. Danach: den Text in eine Datei `LICENSE-EXCEPTION` im Wurzelverzeichnis
   übernehmen und im Lizenzhinweis der betroffenen Crates darauf verweisen.
3. Das muss vor dem ersten fremden Beitrag geschehen. Solange aller Code von
   einer Person stammt, lässt sich die Lizenz noch frei anpassen.
