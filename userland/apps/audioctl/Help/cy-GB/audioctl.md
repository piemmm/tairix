## NAME

audioctl — rhestru'r dyfeisiau sain a newid eu rheolyddion

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

Mae'n rhestru'r allbynnau a'r mewnbynnau y mae'r sesiwn hon yn eu gweld:
dynodwr pob un ar gyfer y cychwyn hwn, ai dyma ragosodiad ei gyfeiriad, ei
lefel a'i fudo, y gyfradd y mae'n rhedeg arni, y fframiau a gollodd, ei
leoliad a'i enw. Mae `streams` yn rhestru eich ffrydiau eich hun, a gyda
`--all` ffrydiau pob prif.

Mae `default`, `level`, `mute` ac `unmute` yn newid rheolyddion un ddyfais.
Enwir dyfais â chyfeiriad `audio:`: `audio:sink/default` neu
`audio:source/default` ar gyfer y rhagosodiad nawr, `audio:sink/<id>` ar gyfer
y cychwyn hwn, neu `audio:sink/<location>` lle bynnag y mae'r ddyfais, fel y
mae'r rhestr yn eu henwi. Mae lefel mewn desibelau i'r canfed, 0 neu is, fel
`-6` neu `-3.5`; nid oes angen `--` ar lefel negyddol.

Mae rheolyddion dyfais yn perthyn i'r ystafell y mae'n ei gwasanaethu. Caiff y
sesiwn sy'n dal yr ystafell honno eu newid, caiff unrhyw un tra bo'r ystafell
heb ei hawlio, ac ni chaiff neb tra bo hi'n cael ei dal yn ôl; mae gwrthodiad
yn dweud hynny. Eiddo sesiwn yw'r hyn y mae'n ei osod: tra bo sesiwn arall yn
dal yr ystafell maent yn camu o'r neilltu, ac maent yn ôl pan ddychwel. Y lefel
y mae pob dyfais yn cychwyn arni a'r dyfeisiau a ffefrir fel rhagosodiadau yw
gosodiadau'r peiriant `audio.level`, `audio.output` ac `audio.input`, a osodir
gan `configure`.

Ar y wybodaeth safonol (fd 3) mae `audioctl streams` yn ysgrifennu cofnod
`omission` pan fydd yn rhestru eich ffrydiau eich hun yn unig.

## OPTIONS

- `-a, --all` — gyda `streams`, ffrydiau pob prif; mae angen `CAP_SYSINFO_GLOBAL`.
- `-h, -?, --help` — dangos cymorth byr y gorchymyn hwn ei hun.
- `--version` — dangos y fersiwn, a gadael.

## EXAMPLES

- `audioctl` — rhestru'r allbynnau a'r mewnbynnau.
- `audioctl level audio:sink/default -10` — gosod yr allbwn rhagosodedig ddeg desibel o dan y llawn.
- `audioctl mute audio:source/default` — mudo'r mewnbwn rhagosodedig.
- `audioctl default audio:sink/2` — gwneud allbwn 2 yn rhagosodiad.
- `audioctl streams --all` — rhestru ffrydiau pob prif.

## EXIT STATUS

- `0` — cwblhawyd y gorchymyn.
- `1` — fe'i gwrthodwyd, neu nid oedd modd ei gyflawni.
- `2` — ni ddeallwyd y llinell orchymyn.

## ENVIRONMENT

- `LANG` — yr iaith a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel `fr-FR`).

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
