## NAME

beeb — efelychydd y BBC Micro Model B

## SYNOPSIS

`beeb`

## DESCRIPTION

Yn rhedeg BBC Micro Model B mewn ffenestr ar y bwrdd gwaith, cylch am gylch
ar ei gyflymder ei hun, gyda'i MOS, ei BBC BASIC a'i DFS ei hun wedi'u
hysgrifennu mewn ystafell lân, fel ei fod yn cychwyn wrth anogwr BASIC gyda
system ffeiliau disg. Llun teledu yw'r llun, pedair uned o led am bob tair o
uchder, ac mae'r sain yn chwarae drwy sain y bwrdd gwaith.

Mae delwedd disg (`.ssd`, `.dsd`) neu dâp (`.uef`, `.csw`) a agorir gyda'r
efelychydd, a ollyngir ar ei ffenestr, neu a ddewisir gyda Mewnosod Disg neu
Mewnosod Tâp yn ei ddewislen yn mynd i'r gyriant neu i'r peiriant casét. Mae
beth bynnag a ysgrifenna'r peiriant ar ddisg yn mynd yn ôl i ffeil y ddisg
tra bo modd ysgrifennu'r ffeil honno; mae disg na ellir ysgrifennu ei ffeil
wedi'i diogelu rhag ysgrifennu. Mae Alldaflu yn tynnu disg allan, ac mae Cadw
Disg Fel yn cadw copi unrhyw le. Caiff tâp a recordiwyd arno ei gadw pan
dynnir ef allan. Mae Cadw Ciplun yn cadw'r peiriant cyfan mewn ffeil, ac mae
Adfer Ciplun yn ei gymryd yn ôl, gyda'i ddisgiau a'i dâp.

Mae'r ddewislen, a agorir drwy glicio'r dde ar y llun neu o eicon yr
efelychydd ar y bar eiconau, hefyd yn cynnig BREAK, Ailosod, Saib a'r
cyflymder; sgrin lawn, graddio miniog neu esmwyth a llinellau sganio; sut y
mapir y bysellfwrdd; a Gludo, sy'n teipio testun y clipfwrdd. Fesul nod, mae'r
bysellau'n teipio'r hyn a ddangosant, a CAPS LOCK y peiriant yn penderfynu a
yw llythyren yn fawr. Fesul safle, pob bysell yw bysell y BBC yn ei lle: F1 i
F9 yw f1 i f9, F10 yw f0, F12 yw BREAK, End yw COPY a Home yw SHIFT LOCK.

Mae Gosodiadau yn dewis beth sydd yn soced y system a'r un ar bymtheg slot
ochr — y ROMau a ddaw gydag ef, RAM ochr, neu ddelweddau ROM y defnyddiwr ei
hun, wedi'u gosod yn storfa'r efelychydd —, modd sgrin, cychwyn ac amseru
disg cysylltiadau'r bysellfwrdd, y llun, y bysellau, y sain a llwytho tâp yn
gyflym, sy'n rhedeg y peiriant mor gyflym ag y gall tra bo modur y casét yn
troi. Daw newidiadau i'r ROMau a'r cysylltiadau i rym ar yr Ailosod nesaf.

Mae lampau ar waelod y ffenestr yn dangos CAPS LOCK, SHIFT LOCK, modur y
casét a phob gyriant. Mae'r peiriant yn rhedeg mewn proses weithio heb unrhyw
awdurdod o gwbl, felly nid ymddiriedir na'i feddalwedd na'i gyfryngau; caiff
proses sy'n methu ei disodli, a'r peiriant yn cychwyn eto fel o'i droi
ymlaen, a dangosir y rheswm.

## OPTIONS

`-h`, `-?`, `--help`
: Ysgrifennu'r cymorth hwn i'r allbwn safonol a gadael.

## EXIT STATUS

Sero unwaith y caeir ei ffenestr neu y dewisir Gadael yn newislen ei eicon ar y bar eiconau. Nid sero pan wrthodwyd sianel y ffenestri, blwch post y digwyddiadau neu sesiwn y bwrdd gwaith; nodir y rheswm ar y ffrwd gwallau safonol.
