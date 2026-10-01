## NAME

Paint — golygydd lluniau a chorluniau graffigol

## SYNOPSIS

`Paint`

## DESCRIPTION

Yn peintio ac yn golygu lluniau mewn ffenestr bwrdd gwaith, fesul picsel
neu gyda brwshys a siapiau. O'i lansio gyda dogfen — o'r rheolwr ffeiliau,
o'r bwrdd gwaith, neu drwy ollwng ffeil ar ei eicon yn y bar eiconau — mae
ef yn agor ffenestr arni. O'i lansio ar ei ben ei hun mae ef yn agor llun
gwyn newydd. Mae pob dogfen yn ffenestr o'r un rhaglen; mae cau'r olaf yn
ei gadael yn y bar eiconau, ac mae rhes Gadael ei ddewislen eicon yn ei
gorffen.

Mae ef yn agor pob fformat llun y mae'r system yn ei ddarllen: PNG, JPEG,
GIF, BMP, TIFF, WebP, eiconau Windows a ffeiliau corluniau RISC OS. Mae ef
yn ysgrifennu ffeiliau PNG, JPEG a chorluniau; caiff llun a ddarllenwyd o
unrhyw fformat arall ei gadw fel ffeil newydd. Mae PNG yn cadw ei balet, ac
mae JPEG yn cael ei ysgrifennu ar yr ansawdd a osodir gydag Ansawdd JPEG yn
y ddewislen Ffeil.

Mae ffeil corluniau'n dal unrhyw nifer o gorluniau, pob un â'i enw, ei fodd
sgrin, ei balet a'i fwgwd. Caiff pob dyfnder ei olygu fel y mae wedi'i
storio: 2, 4, 16 a 256 o liwiau a miliynau o liwiau. Mae corlun heb balet
ei hun yn dangos lliwiau bwrdd gwaith RISC OS — ar gyfer 16 lliw, lliw n
yw lliw Wimp n; ar gyfer 2 liw, lliwiau Wimp 0 a 7; ar gyfer 4 lliw,
lliwiau Wimp 0, 2, 4 a 7; ar gyfer 256 lliw, trefniant arlliwiau RISC OS —
byth balet PC. Mae corlun y mae ei bicseli'n dalach nag y maent yn llydan,
fel yn modd 12, yn cael ei ddangos felly. Caiff corlun na all y golygydd
hwn ei ddarllen, megis un CMYK, ei gadw yn union fel yr oedd a'i gadw'n ôl
heb newid. Mae'r ddewislen Corluniau'n mynd at gorluniau, yn eu hychwanegu,
yn eu copïo, yn eu hailenwi, yn eu dileu ac yn eu haildrefnu.

Mae'r botwm cynradd (chwith) yn peintio â'r lliw cynradd a'r botwm canol
â'r lliw eilaidd; mae dal Alt yn codi lliw yn lle hynny. Yr offer yw dewis,
pensil, brwsh, chwistrell, rhwbiwr, llenwi, codwr lliw, llinell, petryal
ac elips; mae'r panel wrth ymyl y llun yn dal y ddau liw, palet y llun neu
liwiau'r bwrdd gwaith, a gosodiadau'r offeryn sy'n cael ei ddefnyddio. Mae
clicio ar ffynnon liw yn golygu'r lliw hwnnw; mae clicio dwbl ar liw yn y
palet yn golygu'r palet. Mae dal Shift yn tynnu sgwâr, cylch neu linell ar
luosrif o 45 gradd.

Gyda'r offeryn dewis, llusgwch i nodi rhan o'r llun, yna llusgwch y dewis
i'w symud; mae'n arnofio nes iddo gael ei osod i lawr, ac mae ei symud yn
un newid i'w ddadwneud. Mae lluniau a gopïwyd yn teithio drwy'r clipfwrdd
fel PNG, ac mae'r hyn a gludir yn arnofio nes iddo gael ei osod i lawr.

Nid oes gan y rhaglen unrhyw allu ar y system ffeiliau. Dim ond y ffeil a
roddwyd iddi y mae'n ei golygu. Caiff ffeil y caiff y defnyddiwr ei newid
ei rhoi'n ysgrifenadwy, ac mae Cadw'n ei hysgrifennu'n ôl; mae unrhyw un
arall yn ddarllen yn unig, ac mae Cadw'n gofyn ble i gadw copi. Caiff
lluniau, a lluniau a gludwyd o'r clipfwrdd, eu datgodio mewn proses
weithio ar wahân heb unrhyw gyrhaeddiad o gwbl, ac mae pob dogfen yn cael
un newydd: ni all ffeil elyniaethus gyffwrdd â dim y gall y rhaglen ei
gyrraedd.

Mae pwyso botwm eilaidd (de) y llygoden yn unrhyw le yn y ffenestr yn agor
ei dewislen: Torri, Copïo, Gludo, Dewis y cyfan a Dad-ddewis, yna Ffeil,
Golygu, Llun, Lliwiau, Corluniau, Gwedd ac Offer, pob un yn agor ei
is-ddewislen ei hun. Nid oes bar dewislen gan y ffenestr. Mae cau ffenestr
neu adael gyda newidiadau heb eu cadw yn gofyn yn gyntaf.

* `Ctrl+N` — llun newydd; `Ctrl+O` — agor ffeil
* `Ctrl+S` — cadw; `Ctrl+Shift+S` — cadw fel
* `Ctrl+W` — cau'r ffenestr
* `Ctrl+Z` — dadwneud; `Ctrl+Shift+Z` neu `Ctrl+Y` — ailwneud
* `Ctrl+X`, `Ctrl+C`, `Ctrl+V` — torri, copïo, gludo
* `Ctrl+A` — dewis popeth; `Ctrl+D` — dad-ddewis
* `Enter` — gosod dewis sy'n arnofio i lawr; `Escape` — ei roi'n ôl
* `Delete` — clirio'r dewis
* `Ctrl+Shift+X` — tocio i'r dewis
* `Ctrl+R` — newid maint; `Ctrl+Shift+R` — maint y cynfas
* `Ctrl+[` / `Ctrl+]` — cylchdroi i'r chwith neu i'r dde
* `Ctrl+I` — gwrthdroi'r lliwiau
* `S`, `P`, `B`, `A`, `E`, `F`, `I`, `L`, `R`, `O` — yr offer, yn eu trefn
* `X` — cyfnewid y lliwiau cynradd ac eilaidd
* `+` / `-` — chwyddo i mewn neu allan; `1` — maint go iawn; `Ctrl+0` — ffitio
* `G` — dangos neu guddio'r grid rhwng picseli
* `Page Up` / `Page Down` — y corlun cynt neu nesaf
* bysellau saeth — symud dewis sy'n arnofio un picsel; gyda `Shift`, deg

## OPTIONS

`-h`, `-?`, `--help`
: Ysgrifennu'r cymorth hwn i'r allbwn safonol a gadael.

## EXIT STATUS

Sero ar ôl Gadael. Nid sero pan wrthodwyd sianel y ffenestr, blwch post y
digwyddiadau neu sesiwn y bwrdd gwaith; nodir y rheswm ar y ffrwd gwall
safonol.
