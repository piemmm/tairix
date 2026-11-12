## NAME

PCEmulator — efelychydd peiriannau IBM PC a dosbarth XT

## SYNOPSIS

`PCEmulator`

## DESCRIPTION

Mae'n rhedeg cyfrifiaduron personol o oes yr 8088 i'r 80186 mewn ffenestri
bwrdd gwaith: prosesydd, cerdyn dangos, disgiau, sain a phortiau pob peiriant,
ar y cyflymder oedd gan y peiriant.

Mae ffenestr y llyfrgell yn dangos pob peiriant fel teilsen o'i sgrin olaf.
Mae Newydd yn gwneud peiriant o un o bedwar model — yr IBM PC, yr IBM PC XT,
XT turbo a gweithfan 80186 — o dan yr enw a ofynnwyd amdano, a chyda disg
galed 30 MB sy'n cychwyn TAIRiX DOS ar fodel sydd â rheolydd disg galed. Mae
Cychwyn, neu glic dwbl, yn cychwyn y peiriant a ddewiswyd mewn ffenestr ei
hun, neu'n codi ei ffenestr; mae sawl peiriant yn rhedeg ar yr un pryd. Mae
Gosodiadau yn newid caledwedd peiriant tra bo wedi'i ddiffodd, mae Dyblygu yn
copïo peiriant gyda'i ddisgiau, ac mae Dileu yn dileu un gyda'i ddisgiau
unwaith y caiff hynny ei gadarnhau. Mae clicio'r botwm de ar deilsen peiriant yn
cynnig y rhain ar gyfer y peiriant hwnnw, a dewislen y peiriant hefyd tra bo'n
rhedeg. Y tro cyntaf iddo gychwyn, mae PCEmulator
yn gwneud XT turbo. Mae'r hyn y mae peiriant yn ei ysgrifennu i'w ddisgiau, a'r
amser y gosodwyd ei gloc iddo, yn cael eu cadw o un rhediad i'r nesaf.

Mae gan yr XT turbo a'r weithfan 80186 gydbrosesydd rhifiadol 8087, ac mae
Gosodiadau yn gosod un mewn unrhyw beiriant neu'n ei dynnu allan. Mae rhaglen
sy'n aros am y cydbrosesydd heb wneud yn siŵr yn gyntaf fod un yno yn stopio
ar beiriant heb un, fel y gwnâi ar y caledwedd go iawn.

Llun y monitor yw'r llun, pedair uned o led am bob tair o uchder, a phob dot
o'r cerdyn dangos yn cadw ymylon miniog beth bynnag yw maint y ffenestr. Mae
cau ffenestr peiriant yn atal y peiriant dros dro: o'i gychwyn eto, mae'n
ailddechrau lle y stopiodd, gyda'i ddisgiau hyblyg allan o'u gyriannau a'i
gloc wedi symud ymlaen gan yr amser y bu i ffwrdd. Mae caledwedd peiriant sydd
wedi'i atal yn cael ei newid ar ôl ei gychwyn a'i gau i lawr.

Ar waelod ffenestr peiriant, mae gan bob un o'i yriannau lamp sy'n goleuo
tra bo'r gyriant yn gweithio. Mae pwyso gyriant disgiau hyblyg yno yn cynnig
Mewnosod Disg, sy'n dewis delwedd disg hyblyg i'w rhoi yn y gyriant, a Taflu
Allan. Mae delwedd disg hyblyg a ollyngir ar y ffenestr yn mynd i'r gyriant
y caiff ei gollwng arno, neu fel arall i'r cyntaf, ac ni ellir ysgrifennu
iddi yno.

Ar ben draw'r bar hwnnw, mae'r botwm Peiriant yn agor dewislen y peiriant, sy'n
oedi'r peiriant, yn ei ailosod, yn anfon Ctrl+Alt+Delete ato, ac yn gosod
cyflymder ei brosesydd ar gyfer y rhediad. Mae Arddangos yn troi llinellau sganio, tywyn, ôl-olau'r
ffosffor, sŵn a siglo'r llun ymlaen ac i ffwrdd, ac mae'r peiriant yn cadw'r
hyn a ddewisir. Mae Tynnu Sgrinlun yn cadw sgrin y peiriant fel llun PNG lle
mae'r panel Cadw yn ei ddweud. Mae Sgrin Lawn yn llenwi'r dangosydd â'r
ffenestr ac yn gadael y bar yn ei le, lle mae'r un ddewislen yn rhoi'r
dangosydd yn ôl. Mae Cau i Lawr yn diffodd y peiriant, gan gadw ei sgrin olaf
yn unig.

Mae Copïo Testun y Sgrin yn rhoi'r testun y mae sgrin y peiriant yn ei ddangos
ar y clipfwrdd, tra bo'n dangos testun. Mae Gludo yn teipio testun y clipfwrdd
ar fysellfwrdd y peiriant, pob nod unwaith y mae'r rhaglen wedi darllen yr un
blaenorol. Mae'r hyn na all y peiriant ei deipio fel y mae — wynebau a
saethau'r PC, neu lythyren nad yw'n ei hadnabod — yn cael ei adael allan, ac
mae'r ffenestr yn dweud hynny. Mae Stopio Gludo, neu fysell a wasgir yn
ffenestr y peiriant, yn atal y teipio.

Mae Rhannu Ffolder yn rhoi benthyg un o'ch ffolderi i'r peiriant, wedi'i dewis
yn y dewisydd, fel gyriant ei hun: y llythyren gyntaf y mae ei ddisgiau'n ei
gadael yn rhydd, unwaith y bydd TAIRiX DOS wedi cychwyn. Caiff enw na all DOS
ei ddal ei fyrhau, fel y mae Windows yn byrhau enw hir, ac ni ellir newid
ffolder a rannwyd i'w darllen yn unig, na ffeil na chewch ei newid, o'r
peiriant. Mae gan bob ffolder a rannwyd le ar y bar sy'n enwi ei yriant; mae
ei bwyso yn cynnig Stopio Rhannu. Mae peiriant yn rhannu hyd at bedair
ffolder, a dim un unwaith y caiff ei atal dros dro neu ei gau i lawr.

Mae Dadfygiwr yn newislen y peiriant yn agor ffenestr ar y peiriant: ei
gofrestrau, ei 8087, ei god a'i gof fel yr oeddent pan stopiodd, a'i
dorbwyntiau. Mae Torri yn stopio'r peiriant lle y mae; mae Parhau, Cam, Cam
Drosodd a Cam Allan yn ei anfon ymlaen, fel y mae F5, F11, F10 a Shift+F11 yn
ei wneud, ac mae F5 hefyd yn stopio peiriant sy'n rhedeg. Mae pwyso wrth ymyl
llinell o god yn gosod torbwynt yno neu'n ei glirio, fel y mae F9 yn ei wneud
ar gyfer y llinell gyfredol, ac mae Ychwanegu Torbwynt yn stopio'r peiriant yn
lle hynny pan fydd yn ysgrifennu neu'n darllen cof, yn darllen neu'n
ysgrifennu porth, neu'n mynd i mewn i ymyriad. Mae amser y peiriant yn aros yn
llonydd tra bydd wedi stopio, ac mae cau'r dadfygiwr yn gadael iddo fynd
ymlaen.

Tra bo ffenestr peiriant yn dal y bysellfwrdd, mae pob bysell yn mynd i'r
peiriant fel bysell o'i fysellfwrdd ei hun, gan gynnwys Ctrl, Alt a'r bysellau
cloi; mae'r bwrdd gwaith yn cadw Ctrl+Alt+Delete, ei gyfuniadau cipio a'r
fysell Meta. Mae rhaglen sy'n defnyddio'r llygoden drwy yrrwr llygoden DOS yn
dilyn pwyntydd y bwrdd gwaith lle bynnag y mae dros y llun. Mae rhaglen sy'n
darllen y llygoden ei hun yn ei chymryd pan gaiff y llun ei glicio, ac yn ei
rhoi'n ôl pan gaiff y fysell Meta ei tharo'n ysgafn.

Mae cau ffenestr y llyfrgell yn gadael i beiriannau sy'n rhedeg ddal i redeg, ac
mae cau pob ffenestr yn gadael yr efelychydd ar y bar eiconau: mae clic ar ei
eicon, neu Llyfrgell Peiriannau yn ei ddewislen, yn dod â'r llyfrgell yn ôl, ac
mae Gadael yn y ddewislen honno yn atal pob peiriant sy'n rhedeg dros dro ac yn
dod â'r efelychydd i ben. Mae pob peiriant yn rhedeg mewn gweithiwr ei hun nad
oes ganddo unrhyw awdurdod o gwbl: nid oes neb byth yn ymddiried yn ei
feddalwedd na'i ddisgiau.

## OPTIONS

`-h`, `-?`, `--help`
: Ysgrifennu'r cymorth hwn i'r allbwn safonol a gadael.

## EXIT STATUS

Sero unwaith y dewisir Gadael yn ei ddewislen ar y bar eiconau. Heb fod yn sero
pan wrthodwyd sianel y ffenestr, blwch post y digwyddiadau, neu sesiwn y bwrdd
gwaith; nodir y rheswm ar y ffrwd gwallau safonol.
