## NAME

TAIRiXBridge — TAIRiX Bridge, rhaglenni RISC OS ar fwrdd gwaith TAIRiX

## SYNOPSIS

`TAIRiXBridge`

## DESCRIPTION

Mae'n rhedeg rhaglenni RISC OS — rhaglenni BASIC, offer llinell orchymyn a
gemau un dasg — ar fwrdd gwaith TAIRiX, pob un yn dangos ei sgrin mewn ffenestr
ei hun. TAIRiX Bridge biau'r RISC OS y maen nhw'n rhedeg arno, wedi'i
ysgrifennu o ddogfennaeth gyhoeddedig; nid oes ynddo god RISC OS.

Mae rhaglenni'n rhedeg mewn sesiynau. Y sesiwn a rennir yw'r peiriant RISC OS
y mae ffenestr TAIRiX Bridge yn ei ddangos: ei ddisg ei hun yw'r ffolder RISC
OS yn eich Dogfennau, mae ei ddilyniant cychwyn yn rhedeg wrth iddo gychwyn,
ac mae rhaglen o'ch storfa eich hun yn rhedeg ynddo. Mae rhaglen o rywle arall
— cyfryngau symudadwy, delwedd disg, cyfran rhwydwaith — neu o le anhysbys yn
rhedeg mewn sesiwn ynysig ei hun, nad yw byth yn gweld eich ffolder RISC OS ac
sy'n dod i ben gyda'i rhaglen olaf. Mae Rhedeg yn y Sesiwn a Rennir yn rhedeg
ffeil yn y sesiwn a rennir, o ble bynnag y daeth.

Mae ffenestr sesiwn yn rhestru ei thasgau, pob un gyda'i chof ac a yw wedi'i
seibio neu'n brysur, a'r hyn y mae ardaloedd a rennir y sesiwn yn ei ddal. Mae Rhedeg yn
dewis ffeil i'w rhedeg; mae ffeil a ollyngir ar y ffenestr, neu a agorir gyda
TAIRiX Bridge, yn rhedeg hefyd. Mae dewis tasg ac yna Seibio neu Gadael yn ei
dal neu'n ei gorffen.

Mae sgrin pob rhaglen yn ffenestr ei hun, a'i llun wedi'i raddio i'r ffenestr
yng nghyfrannau ei modd. Mae'r botwm Sgrin yn agor dewislen y ffenestr: Sgrin
Lawn, graddio Miniog neu Llyfn, Seibio, ac Ailosod, sy'n gorffen y rhaglen ac
yn ei rhedeg eto yn yr un ffenestr. Mae'r sgrin yn cymryd y bysellfwrdd cyfan,
pob bysell gyda'r rhif y mae RISC OS yn ei roi iddi. Select, Menu ac Adjust yw'r
botymau cynradd, eilaidd a chanol. Unwaith y bydd rhaglen yn symud y llygoden
ei hun, mae clic yn dal y llygoden iddi, ac mae un tap ar y fysell Meta yn ei
rhyddhau.

Mae Gosodiadau yn dewis trefn y botymau, y cof y caiff pob sesiwn ei gymryd, a
yw pob rhaglen wedi'i hynysu, a sut y caiff sgriniau eu graddio. Nid oes gan
broses waith sesiwn unrhyw awdurdod: caiff y ffeiliau y mae'n eu darllen a'u
hysgrifennu eu cludo drosti, ac mae proses sy'n methu yn gorffen ei sesiwn,
a'r rheswm yn cael ei ddangos yn ffenestr y sesiwn.

## OPTIONS

`-h`, `-?`, `--help`
: Ysgrifennu'r cymorth hwn i'r allbwn safonol a gadael.

## EXIT STATUS

Sero unwaith y caiff ffenestr y sesiwn olaf ei chau. Nid sero pan wrthodwyd y
sianel ffenestri, y blwch digwyddiadau neu sesiwn y bwrdd gwaith; nodir y
rheswm ar y ffrwd wallau safonol.
