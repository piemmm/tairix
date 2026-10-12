## NAME

netmap — map a rhestr o'r rhwydwaith

## SYNOPSIS

`netmap`

## DESCRIPTION

Yn agor ffenestr sy'n mapio'r rhwydweithiau y mae'r peiriant hwn wedi'i
gysylltu â nhw: ei ryngwynebau rhwydwaith, yr is-rwydweithiau y maen nhw arnyn
nhw, y gwesteiwyr a welodd ei storfa cymdogion ar bob un, y llwybryddion y mae
ei lwybrau'n mynd drwyddynt, a'r porth rhagosodedig sy'n arwain y tu hwnt i'r
rhwydwaith lleol. Mae pob un wedi'i dynnu fel eicon ar ddisg o liw'r hyn ydyw
— cyfrifiaduron yn las, llwybryddion yn oren, argraffwyr yn borffor, storfa yn
wyrddlas, ffonau yn wyrdd — ac mae pob llinell yn cymryd lliw'r hyn y mae'n
arwain ato. Mae gwesteiwr y mae'r map yn adnabod ei system weithredu yn
gwisgo'i harwyddlun, ac mae golau wrth ymyl gwesteiwr neu ryngwyneb yn dangos
a oedd modd ei gyrraedd y tro diwethaf y'i gwelwyd (gwyrdd) ai peidio (coch).

Llusgwch â'r prif fotwm, trowch yr olwyn, neu sgroliwch â dau fys ar bad
cyffwrdd i symud o amgylch y map. Daliwch `Ctrl` a throi'r olwyn, neu binsio
ar bad neu sgrin gyffwrdd, i chwyddo o amgylch y pwynt o dan y pwyntydd;
pwyswch `Home` i ffitio'r map cyfan yn y ffenestr eto. Cliciwch nod i weld yr
hyn sy'n hysbys amdano — ei gyfeiriadau, ei gyfeiriad caledwedd a'i
wneuthurwr, a oes modd ei gyrraedd, pa fath o ddyfais y mae'n ymddangos ei fod
a beth sy'n rhedeg arno, gyda pha mor sicr yw'r map a pham — a chliciwch le
gwag i gau'r manylion. Mae enwau'n ymddangos lle bynnag y mae lle iddynt.

Darllenir y map drwy'r API Gwybodaeth System a'r gwasanaeth darganfod, a
rhoddir enwau i'w westeiwyr gan y gweinyddion DNS sydd wedi'u gosod neu gan yr
enw y mae'r dyfeisiau'n ei roi arnynt eu hunain; nid yw byth yn cysylltu â
gwesteiwr ei hun, ac fe'i darllenir eto bob ychydig eiliadau tra bo'i ffenestr
ar agor. Wrth ymyl y gwesteiwyr mae'n dangos y gwasanaethau y mae dyfeisiau'n
eu hysbysebu ar y cyswllt lleol — argraffwyr, ffolderi a rennir, chwaraewyr
cyfryngau — pob un o dan y gwesteiwr sy'n ei gynnig. Mae cyfeiriadau
rhyngwyneb a llwybrau angen `CAP_SYSINFO_GLOBAL`; mae cyfeiriadau caledwedd a
storfa'r cymdogion angen `CAP_SYSINFO_HW`; mae gwasanaethau a hysbysebir angen
`CAP_NET_DISCOVER_ALL`; mae enwi gwesteiwyr drwy DNS angen `CAP_NET`. Heb un
ohonynt, mae'r map yn dweud beth na all ei ddangos ac yn llunio'r gweddill.

Lansir netmap o Lyfrgell Rhaglenni'r bwrdd gwaith neu wrth ei enw o gragen, ac
mae angen sesiwn graffigol yn rhedeg arno; hebddi, mae'n adrodd y gwrthodiad
ar y ffrwd gwallau safonol ac yn gadael.

## EXIT STATUS

Sero ar ôl Gadael; nid sero pan wrthodwyd sianel y ffenestr neu'r rhanbarth
ffrâm a rennir (nodir y rheswm ar y ffrwd gwallau safonol).
