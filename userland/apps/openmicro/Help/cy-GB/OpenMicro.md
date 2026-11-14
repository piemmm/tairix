## NAME

OpenMicro — Open Micro Emulator, efelychydd microgyfrifiaduron 8-did

## SYNOPSIS

`OpenMicro`

## DESCRIPTION

Mae'n rhedeg microgyfrifiaduron 8-did mewn ffenestri bwrdd gwaith — y BBC Micro
Model B a'r Acorn Electron — cylch am gylch ar eu cyflymder eu hunain, a phob
un wedi'i gyfarparu â'i system weithredu a'i BASIC ei hun, a ysgrifennwyd mewn
ystafell lân, a'r Model B â system ffeiliau disg hefyd, fel bod peiriant yn
cychwyn wrth anogwr BASIC. Llun teledu yw'r llun, pedair uned o led am bob tair
o uchder, ac mae'r sain yn chwarae drwy sain y bwrdd gwaith.

Mae ffenestr y llyfrgell yn dangos pob peiriant fel teilsen o'i sgrin olaf. Mae
Newydd yn gwneud peiriant, o dan yr enw a ofynnwyd amdano: BBC Micro Model B,
gyda disg wag newydd yng ngyriant 0 neu heb un, neu Acorn Electron, gyda Plus 1
wedi'i osod neu hebddo. Mae Cychwyn, clic dwbl neu'r fysell Return yn cychwyn y
peiriant a ddewiswyd mewn ffenestr ei hun, neu'n codi ei ffenestr; cedwir hyd
at ugain peiriant, ac mae sawl un yn rhedeg ar yr un pryd. Mae Dyblygu yn copïo
peiriant gyda'i gyflwr a gadwyd a'i ffolder, ac mae Dileu yn dileu peiriant a'i
gyflwr a gadwyd unwaith y caiff hynny ei gadarnhau, gan adael ei ffolder. Mae
clicio'r botwm de ar deilsen peiriant yn cynnig Cychwyn, Gosodiadau, Dyblygu a
Dileu ar gyfer y peiriant hwnnw, a dewislen y peiriant hefyd tra bo'n rhedeg. Y
tro cyntaf iddo gychwyn, mae'r efelychydd yn gwneud BBC Micro Model B gyda disg
wag.

Mae Gosodiadau yn newid enw peiriant, ei lefel sain a llwytho tâp yn gyflym,
sy'n rhedeg y peiriant mor gyflym ag y gall tra bo modur y casét yn troi. Dim
ond tra bo wedi'i gau i lawr y newidir yr hyn sydd wedi'i osod ynddo: y ROM ym
mhob soced — OS y Model B a'i un ar bymtheg slot ochr, OS a BASIC yr Electron,
a ROM a chetris ei Plus 1 — pob un yn ROM a ddaw gyda'r efelychydd, RAM ochr
mewn slot Model B, dim byd mewn slot neu getrisen, neu eich delwedd ROM eich
hun, wedi'i rhoi yn storfa'r efelychydd gyda Gosod Delwedd; Plus 1 yr Electron;
a chysylltiadau bysellfwrdd y Model B, sy'n gosod ei fodd sgrin wrth gychwyn,
sut mae disg yn cychwyn, ac amseru'r disg.

Mae cau ffenestr peiriant yn atal y peiriant dros dro: o'i gychwyn eto, mae'n
ailddechrau lle y stopiodd. Mae Cau i Lawr yn ei ddewislen yn tynnu ei ddisgiau
a'i dâp allan ac yn ei ddiffodd, gan gadw ei sgrin olaf yn unig.

Mae gan bob peiriant ffolder ei hun, Open Micro, yna enw'r peiriant, yn eich
ffolder Documents. Mae Disg Newydd a Tâp Newydd yn gwneud disg neu dâp gwag yno
ac yn ei roi i mewn, ac mae O'r Ffolder yn cynnig y disgiau a'r tapiau sydd yno
eisoes. Mae'r hyn y mae ei yriannau a'i beiriant casét yn ei ddal o'i ffolder
yn aros gyda'r peiriant: o'i gau i lawr, mae'n eu cymryd i mewn eto pan fydd yn
cychwyn; o'i ailddechrau, mae'n ysgrifennu'n ôl iddynt dim ond lle nad yw
unrhyw raglen arall wedi'u newid ers iddo gael ei atal dros dro.

Mae delwedd disg (`.ssd`, `.dsd`) neu ddelwedd tâp (`.uef`, `.csw`) a agorir
gyda'r efelychydd yn mynd i'r peiriant sy'n rhedeg y bu ei ffenestr yn
weithredol ddiwethaf, neu i beiriant arall sy'n rhedeg ac sy'n ei derbyn, neu
fel arall i'r peiriant cyntaf yn y llyfrgell sy'n ei derbyn, a gychwynnir ar ei
chyfer. Mae un a ddewisir gyda Mewnosod Disg neu Mewnosod Tâp yn mynd i'r
gyriant hwnnw neu i'r peiriant casét, ac mae un a ollyngir ar ffenestr peiriant
yn mynd i yriant 0 neu i'r peiriant casét, wedi'i diogelu rhag ysgrifennu. Mae
beth bynnag a ysgrifenna'r peiriant ar ddisg yn mynd yn ôl i ffeil y ddisg tra
bo modd ysgrifennu'r ffeil honno; mae disg na ellir ysgrifennu ei ffeil wedi'i
diogelu rhag ysgrifennu. Mae Alldaflu yn tynnu disg allan, ac mae Cadw Disg Fel
yn cadw copi unrhyw le. Caiff tâp a recordiwyd arno ei gadw pan dynnir ef
allan. Mae Cadw Ciplun yn cadw'r peiriant cyfan mewn ffeil, ac mae Adfer Ciplun
yn ei gymryd yn ôl, gyda'i ddisgiau a'i dâp.

Mae'r ddewislen, a agorir drwy glicio'r dde ar y llun, yn cynnig pob gyriant ar
Model B a'r tâp; BREAK, Ailosod, Saib a'r cyflymder; sgrin lawn, graddio miniog
neu esmwyth a llinellau sganio; sut y mapir y bysellfwrdd; Gludo, sy'n teipio
testun y clipfwrdd; y cipluniau; Gosodiadau; Llyfrgell Peiriannau; a Cau i
Lawr. Fesul nod, mae'r bysellau'n teipio'r hyn a ddangosant, a CAPS LOCK y
peiriant yn penderfynu a yw llythyren yn fawr. Fesul safle, pob bysell yw
bysell y peiriant yn ei lle. Ar Model B, F1 i F9 yw f1 i f9, F10 yw f0, Home yw
SHIFT LOCK ac Insert yw'r fysell ôl-slaes. Ar Electron, F1 i F10 yw FUNC gydag
1 i 9 a 0, Alt yw FUNC, a Caps Lock yw CAPS LK. Ar y ddau, F12 yw BREAK, End yw
COPY a'r saethau yw bysellau'r cyrchwr.

Mae lampau ar waelod ffenestr peiriant yn dangos CAPS LOCK a modur y casét, ac
ar Model B, SHIFT LOCK a phob gyriant. Mae pob peiriant yn rhedeg mewn proses
weithio ei hun heb unrhyw awdurdod o gwbl, felly nid ymddiriedir na'i
feddalwedd na'i gyfryngau; caiff proses sy'n methu ei disodli, a'r peiriant yn
cychwyn eto fel o'i droi ymlaen, a dangosir y rheswm.

Mae cau ffenestr y llyfrgell yn gadael i beiriannau sy'n rhedeg ddal i redeg,
ac mae cau pob ffenestr yn gadael yr efelychydd ar y bar eiconau: mae clic ar
ei eicon, neu Llyfrgell Peiriannau yn ei ddewislen, yn dod â'r llyfrgell yn ôl,
ac mae Gadael yn y ddewislen honno yn atal pob peiriant sy'n rhedeg dros dro ac
yn dod â'r efelychydd i ben.

## OPTIONS

`-h`, `-?`, `--help`
: Ysgrifennu'r cymorth hwn i'r allbwn safonol a gadael.

## EXIT STATUS

Sero unwaith y dewisir Gadael yn newislen ei eicon ar y bar eiconau. Nid sero
pan wrthodwyd sianel y ffenestri, blwch post y digwyddiadau neu sesiwn y bwrdd
gwaith; nodir y rheswm ar y ffrwd gwallau safonol.
