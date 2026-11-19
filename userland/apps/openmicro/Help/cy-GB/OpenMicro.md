## NAME

OpenMicro — Open Micro Emulator, efelychydd microgyfrifiaduron 8-did

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Mae'n rhedeg microgyfrifiaduron 8-did mewn ffenestri bwrdd gwaith — y BBC Micro
Model B, yr Acorn Electron a'r Commodore 64 — cylch am gylch ar eu cyflymder eu
hunain, a phob un wedi'i gyfarparu â'i system weithredu a'i BASIC ei hun, a
ysgrifennwyd mewn ystafell lân, a'r Model B â system ffeiliau disg a'r
Commodore 64 â gyriant disg 1541 gyda'i DOS ei hun, fel bod peiriant yn cychwyn
wrth anogwr BASIC. Llun teledu yw'r llun, pedair uned o led am bob tair o
uchder, ac mae'r sain yn chwarae drwy sain y bwrdd gwaith.

Mae ffenestr y llyfrgell yn dangos pob peiriant fel teilsen o'i sgrin olaf,
neu o sgrin gychwyn ei fodel nes iddo redeg. Mae Newydd yn gwneud peiriant, o
dan yr enw a ofynnwyd amdano: BBC Micro Model B, gyda RAM ochr yn slotiau
4 i 7 a disg wag newydd yng ngyriant 0 neu heb un; Acorn Electron, gyda Plus 1
wedi'i osod neu hebddo; neu Commodore 64, PAL neu NTSC, gyda SID 6581 neu 8580
ac un gyriant 1541, dau neu ddim un. Mae Cychwyn, clic dwbl neu'r fysell
Return yn cychwyn y peiriant a ddewiswyd mewn ffenestr ei hun, neu'n codi ei
ffenestr; cedwir hyd at ugain peiriant, ac mae sawl un yn rhedeg ar yr un
pryd. Mae Dyblygu yn copïo peiriant gyda'i gyflwr a gadwyd a'i ffolder, ac mae
Dileu yn dileu peiriant a'i gyflwr a gadwyd unwaith y caiff hynny ei
gadarnhau, gan adael ei ffolder. Mae clicio'r botwm de ar deilsen peiriant yn
cynnig Cychwyn, Gosodiadau, Dyblygu, Dileu a Dangos Ffolder ar gyfer y
peiriant hwnnw, a dewislen y peiriant hefyd tra bo'n rhedeg. Y tro cyntaf iddo
gychwyn, mae'r efelychydd yn gwneud BBC Micro Model B gyda disg wag.

Mae Gosodiadau yn newid enw peiriant, ei lefel sain, llwytho cyflym, sy'n
rhedeg y peiriant mor gyflym ag y gall tra bo modur y casét yn troi neu ddisg
yn troi mewn gyriant a'r peiriant yn dawel, ac ar Commodore 64 y porth rheoli y
mae ffon reoli'r bysellbad ynddo. Dim ond tra bo wedi'i gau i lawr y newidir yr
hyn sydd wedi'i osod ynddo: y ROM ym mhob soced — OS y Model B a'i un ar
bymtheg slot ochr, OS a BASIC yr Electron, ROM a chetris ei Plus 1, a KERNAL,
BASIC a nodau'r Commodore 64 a DOS pob un o'i yriannau — pob un yn ROM a ddaw
gyda'r efelychydd, RAM ochr mewn slot Model B, dim byd mewn slot neu getrisen,
neu eich delwedd ROM eich hun, wedi'i rhoi yn storfa'r efelychydd gyda Gosod
Delwedd; Plus 1 yr Electron; safon deledu, SID a gyriannau'r Commodore 64; a
chysylltiadau bysellfwrdd y Model B, sy'n gosod ei fodd sgrin wrth gychwyn, sut
mae disg yn cychwyn, ac amseru'r disg. Mae ychydig o raglenni yn darllen cod y
ROMau gwreiddiol eu hunain, a dim ond gyda delweddau o'r rheini wedi'u gosod y
maent yn rhedeg.

Mae gan Model B hefyd VIM, golygydd yn null vi, wedi'i osod yn slot 13. Wrth
anogwr y peiriant, mae `*VIM` ac enw ffeil yn golygu'r ffeil honno, ac mae
BREAK yn VIM yn cadw'r testun sydd ynddo. Mae VIM yn golygu testun, megis y
ffeiliau `!BOOT` y mae `*BUILD` yn eu gwneud, a rhaglenni BASIC, y mae'n eu
rhestru fel testun ac yn eu cadw eto wedi'u tocyneiddio, gan liwio pob un fel y
mae lliwiau'r modd sgrin yn caniatáu. Yn VIM, mae `:help` yn rhestru ei
orchmynion a'r moddau sgrin gyda'r lle y mae pob un yn ei adael yn y prif gof,
mae `:mode` a rhif yn newid y modd, ac mae `:q` yn mynd yn ôl i BASIC. Unwaith
y bydd y prif gof yn llawn, mae VIM yn mynd ymlaen i fanciau o RAM ochr nad oes
ROM ynddynt, hyd at ddau, fel y gall olygu ffeil sy'n fwy na'r prif gof; mae
ffeil o'r fath yn cymryd mwy o amser i'w darllen a'i hysgrifennu.

Mae cau ffenestr peiriant yn atal y peiriant dros dro: o'i gychwyn eto, mae'n
ailddechrau lle y stopiodd. Mae Cau i Lawr yn ei ddewislen yn tynnu ei
gyfryngau allan ac yn ei ddiffodd, gan gadw ei sgrin olaf yn unig.

Mae gan bob peiriant ffolder ei hun, Open Micro, yna enw'r peiriant, yn eich
ffolder Documents. Mae Disg Newydd, ar Commodore 64 hefyd, a Tâp Newydd yn
gwneud disg neu dâp gwag yno ac yn ei roi i mewn, mae O'r Ffolder yn cynnig y
disgiau a'r tapiau sydd yno eisoes, ac mae Cadw Ciplun yn cadw'r peiriant
cyfan yno fel ffeil `.snapshot`. Mae Dangos Ffolder yn agor y ffolder yn
Files, fel y gellir copïo'r hyn y mae'r peiriant yn ei gadw yno i gyfryngau
eraill neu i gyfrifiadur arall. Mae'r hyn y mae ei yriannau a'i beiriant
casét yn ei ddal o'i ffolder yn aros gyda'r peiriant: o'i gau i lawr, mae'n eu
cymryd i mewn eto pan fydd yn cychwyn; o'i ailddechrau, mae'n ysgrifennu'n ôl
iddynt dim ond lle nad yw unrhyw raglen arall wedi'u newid ers iddo gael ei
atal dros dro.

Mae delwedd disg (`.ssd`, `.dsd`) neu ddelwedd tâp (`.uef`, `.csw`), neu ddisg
(`.d64`, `.g64`), tâp (`.tap`), cetrisen (`.crt`) neu raglen (`.prg`, `.t64`)
ar gyfer Commodore 64, a agorir gyda'r efelychydd yn mynd i'r peiriant sy'n
rhedeg y bu ei ffenestr yn weithredol ddiwethaf, neu i beiriant arall sy'n
rhedeg ac sy'n ei derbyn, neu fel arall i'r peiriant cyntaf yn y llyfrgell sy'n
ei derbyn, a gychwynnir ar ei chyfer, a daw ffenestr y peiriant hwnnw i'r
blaen. Mae agor disg BBC Micro yn Files yn dangos y ffeiliau arni yn lle
cychwyn peiriant. Mae un a ddewisir gyda Mewnosod Disg neu Mewnosod Tâp yn
mynd i'r gyriant hwnnw neu i'r peiriant casét, ac mae un a ollyngir ar
ffenestr peiriant yn mynd i'w yriant cyntaf neu i'r peiriant casét,
wedi'i diogelu rhag ysgrifennu, a'r ffenestr yn cymryd y bysellfwrdd. Mae beth
bynnag a ysgrifenna'r peiriant ar ddisg yn mynd yn ôl i ffeil y ddisg tra bo
modd ysgrifennu'r ffeil honno, a thra bo'r ddisg yn y gyriant ni chaiff na
gosodiad na pheiriant arall ysgrifennu'r ffeil; mae disg na ellir ysgrifennu
ei ffeil, neu y mae rhaglen arall yn ei defnyddio, wedi'i diogelu rhag
ysgrifennu, ac mae'r ffenestr yn dweud pam. Os bydd angen `.g64` ar ddisg
`.d64` bellach, cynigir ei chadw fel un. Mae Alldaflu yn tynnu disg allan, ac
mae Cadw Disg Fel yn cadw copi unrhyw le. Caiff tâp a recordiwyd arno ei gadw
pan dynnir ef allan. Ar Commodore 64, mae Chwarae, Recordio, Stopio ac
Ailddirwyn yn y ddewislen Tâp yn gweithredu'r datasette; mae Mewnosod Cetrisen
yn plygio cetrisen i mewn ac mae Tynnu Cetrisen yn ei thynnu allan, gyda'r
naill a'r llall yn ailgychwyn y peiriant; ac mae Llwytho Rhaglen yn rhoi
rhaglen yn y cof, fel y gwnâi LOAD, i RUN ei chychwyn. Mae Cadw Ciplun yn
cadw'r peiriant cyfan mewn ffeil — yn ei ffolder, neu lle y dewiswch cyn bod
ganddo un — ac mae Adfer Ciplun yn cymryd peiriant a gadwyd felly yn ôl,
gyda'i gyfryngau.

Mae'r ddewislen, a agorir drwy glicio'r dde ar y llun, yn cynnig pob gyriant ar
Model B neu Commodore 64 a'r tâp, ac ar Commodore 64 y getrisen a Llwytho
Rhaglen; BREAK, neu RESTORE ar Commodore 64; ar Model B ac Electron,
SHIFT-BREAK, sy'n pwyso BREAK gyda SHIFT wedi'i dal i lawr i gychwyn y ddisg
neu'r tâp; Ailosod, Saib a'r cyflymder; sgrin lawn, graddio miniog neu esmwyth
a llinellau sganio; sut y mapir y bysellfwrdd; Gludo, sy'n teipio testun y
clipfwrdd, pob llythyren yn fawr neu'n fach fel y mae'r peiriant yn ei dangos;
y cipluniau; Dangos Ffolder; Gosodiadau; Llyfrgell Peiriannau; a Cau i Lawr.
Fesul nod, mae'r bysellau'n teipio'r hyn a ddangosant, a CAPS LOCK y peiriant,
neu Shift ar Commodore 64, yn penderfynu a yw llythyren yn fawr. Fesul safle,
pob bysell yw bysell y peiriant yn ei lle. Ar Model B, F1 i F9 yw f1 i f9, F10
yw f0, Home yw SHIFT LOCK ac Insert yw'r fysell ôl-slaes. Ar Electron, F1 i
F10 yw FUNC gydag 1 i 9 a 0, Alt yw FUNC, a Caps Lock yw CAPS LK. Ar y ddau,
F12 yw BREAK, End yw COPY a'r saethau yw bysellau'r cyrchwr. Ar Commodore 64,
F1, F3, F5 ac F7 yw ei fysellau swyddogaeth ac F2, F4, F6 ac F8 yw'r un
bysellau gyda SHIFT, Escape yw RUN/STOP, Alt yw'r fysell Commodore, Ctrl yw
CTRL, Home yw CLR/HOME, F12 neu Page Up yw RESTORE, a'r saethau yw bysellau'r
cyrchwr; gyda ffon reoli'r bysellbad ymlaen, mae saethau'r bysellbad rhifol yn
symud y ffon reoli ac mae 0 a 5 yn tanio.

Mae lampau ar waelod ffenestr peiriant yn dangos CAPS LOCK a modur y casét, ac
ar Model B, SHIFT LOCK a phob gyriant; ar Commodore 64 maent yn dangos modur y
datasette a phob gyriant. Ar Model B ac Electron, mae'r botwm SHIFT-BREAK ar
ochr dde'r gwaelod yn gwneud yr hyn y mae rhes y ddewislen yn ei wneud. Mae
pob peiriant yn rhedeg mewn proses weithio ei hun heb unrhyw awdurdod o gwbl,
felly nid ymddiriedir na'i feddalwedd na'i gyfryngau; caiff proses sy'n methu
ei disodli, a'r peiriant yn cychwyn eto fel o'i droi ymlaen, a dangosir y
rheswm.

Mae cau ffenestr y llyfrgell yn gadael i beiriannau sy'n rhedeg ddal i redeg,
ac mae cau pob ffenestr yn gadael yr efelychydd ar y bar eiconau: mae clic ar
ei eicon, neu Llyfrgell Peiriannau yn ei ddewislen, yn dod â'r llyfrgell i'r
blaen bob amser, ac mae Gadael yn y ddewislen honno yn atal pob peiriant sy'n
rhedeg dros dro ac yn dod â'r efelychydd i ben.

## OPTIONS

`-h`, `-?`, `--help`
: Ysgrifennu'r cymorth hwn i'r allbwn safonol a gadael.

## EXIT STATUS

Sero unwaith y dewisir Gadael yn newislen ei eicon ar y bar eiconau. Nid sero
pan wrthodwyd sianel y ffenestri, blwch post y digwyddiadau neu sesiwn y bwrdd
gwaith; nodir y rheswm ar y ffrwd gwallau safonol.
