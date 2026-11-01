## NAME

mount — rhestru'r systemau ffeiliau wedi'u gosod, neu osod delwedd ddisg

## SYNOPSIS

`mount`

`mount [option...] image [target]`

## DESCRIPTION

Heb operandau, mae'n rhestru'r systemau ffeiliau wedi'u gosod, un
llinell `source on target type fstype (options)` fesul man lle
dangosir cyfrol. Mae `-t` wedyn yn rhestru dim ond y mathau y mae ei
restr wedi'i hwahanu gan gomâu yn eu henwi; mae rhestr sy'n dechrau â
`no` yn rhestru pob math nad yw'n ei enwi.

Gydag operand `image`, mae'n gosod y gyfrol y mae'r ffeil delwedd
ddisg honno'n ei dal: disg ISO 9660 neu UDF (`.iso`), disg hyblyg
Acorn ADFS neu AmigaDOS (`.adf`, `.adl`, `.adm`, `.ads`), rhaniad
disg galed AmigaDOS (`.hdf`), neu gyfrol FAT32. Adnabyddir yr hyn y
mae'r ddelwedd yn ei ddal o'i chynnwys, byth o'i henw. Gosodir y
gyfrol ar eich cyfer chi yn unig, yn `/Storage/name`, dan enw a
gymerir o label y gyfrol neu, fel arall, o enw ffeil y ddelwedd; ni
chaiff neb arall ei hagor. Fe'i gosodir i'w darllen a'i hysgrifennu
lle cewch ysgrifennu i'r ddelwedd ac y gellir ysgrifennu ei fformat,
ac i'w darllen yn unig fel arall, rhywbeth y mae `mount` yn
rhybuddio amdano. Nid yw cyfrol a osodwyd o ddelwedd byth yn rhedeg
rhaglenni.

Mae `target` ar ffurf `/Storage/name` yn gosod y gyfrol dan yr enw
hwnnw. Mae unrhyw `target` arall, cyfeiriadur gwag sy'n eiddo i chi
dan `/Users` neu ar gyfrol yn `/Storage`, yn dangos y gyfrol yno
hefyd; ni all cyfeiriadur ar gyfrol FAT32 neu ADFS ei dangos.

Mae'r gyfrol yn aros wedi'i gosod nes i chi ei datgysylltu gyda
`unmount`, neu nes i'ch sesiwn fewngofnodi ddod i ben. Ni osodir
delwedd sy'n dal disg wedi'i rhannu'n rhaniadau, un ddisg o arae
RAID, neu system ffeiliau y caiff ei pherchnogion a gofnodwyd eu
parchu bob amser (ext2, ext3, ext4, ARXFS), ac mae `mount` yn dweud
pam.

## OPTIONS

- `-r, --read-only` — gosod y gyfrol i'w darllen yn unig (fel `-o ro`).
- `-w, --rw, --read-write` — gosod y gyfrol i'w darllen a'i
  hysgrifennu, neu ddim o gwbl: ni osodir delwedd na chewch ysgrifennu
  iddi, neu na ellir ysgrifennu i'w system ffeiliau. Mae `-r`
  diweddarach yn tynnu hyn yn ôl; nid yw `-o ro` nac `-o rw`
  diweddarach yn gwneud hynny. Yr olaf o `-r`, `-w`, `-o ro` ac
  `-o rw` sy'n penderfynu a yw'r gyfrol i'w darllen yn unig.
- `-t, --types <list>` — y systemau ffeiliau, wedi'u gwahanu â chomâu,
  y mae'n rhaid i'r ddelwedd ddal un ohonynt, fel y mae'r rhestr
  osodiadau'n eu henwi (`iso9660`, `udf`, `adfs`, `affs`, `fat32`), neu
  `auto` ar gyfer beth bynnag y mae'n ei ddal; mae rhestr sy'n dechrau
  â `no` yn enwi'r rhai na chaiff y ddelwedd eu dal. Adnabyddir yr hyn y
  mae'r ddelwedd yn ei ddal o'i chynnwys, felly nid yw'r drefn yn dewis
  dim: fel UDF y gosodir disg sy'n dal UDF yn ogystal ag ISO 9660, oni
  bai bod yr ISO 9660 arni yn cadw cofnodion Rock Ridge neu RISC OS. Ni
  osodir delwedd nad yw'n dal yr un ohonynt, na rhestr nad yw'n enwi'r
  un math y mae delwedd ddisg yn ei ddal.
- `-o, --options <list>` — rhestr wedi'i gwahanu â choma o `ro`, `rw`
  (i'w darllen a'i hysgrifennu lle bo modd, y rhagosodiad), `noexec`,
  `loop` a `name=name`, yr enw y mae'r gyfrol i'w gymryd. Fel `noexec`
  a `loop` y gosodir delwedd beth bynnag.
- `-v, --verbose` — dweud ble y gosodwyd y gyfrol.
- `-l, --show-labels` — derbynnir ar gyfer y rhestr, nad oes ganddi
  labeli i'w hychwanegu.
- `-h, -?, --help` — dangos cymorth byr y gorchymyn hwn ei hun.

## EXAMPLES

- `mount` — rhestru'r systemau ffeiliau wedi'u gosod.
- `mount Workbench.adf` — gosod delwedd ddisg hyblyg Amiga, yn
  `/Storage/workbench` os mai dyna'i label.
- `mount -r disc.iso /Storage/disc` — gosod delwedd ddisg i'w darllen
  yn unig fel `disc`.
- `mount -v -t adfs Elite.adl` — gosod delwedd ddisg hyblyg Acorn, gan
  ddweud ble.

## EXIT STATUS

- `0` — ysgrifennwyd y rhestr, neu gosodwyd y gyfrol (neu ysgrifennwyd
  y cymorth byr).
- `1` — ni ddeallwyd y llinell orchymyn.
- `2` — ni ellid darllen y rhestr osodiadau, neu ni ellid ysgrifennu
  i'r derfynell.
- `32` — ni osodwyd y ddelwedd.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `fr-FR`).

## SEE ALSO

- `unmount`
- `df`
- `man`
