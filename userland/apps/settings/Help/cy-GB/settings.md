## NAME

settings — cyflunio'r bwrdd gwaith a'r peiriant hwn

## SYNOPSIS

`settings`

## DESCRIPTION

Yn agor ffenestr bwrdd gwaith sy'n rhestru pob categori o osodiadau sydd gan y
system hon: beth yw'r peiriant, sut mae'r bwrdd gwaith yn edrych, y sgrin, y
rhwydwaith, y dyfeisiau y mae'n cael ei yrru â hi, y cyfrifon sy'n ei
ddefnyddio, a'r cyfrolau y mae'n eu dal. Mae dewis categori o'r bar ochr yn
dangos ei gwarel. Yn lle hynny, mae categori sy'n dal sawl gwarel yn agor ac yn
cau eu rhestr yn ei lle, a gall unrhyw nifer o'r rhestrau hynny fod ar agor ar
yr un pryd.

Nid yw Settings yn dal unrhyw awdurdod ei hun. Mae pob newid yn gais i'r sesiwn
bwrdd gwaith, sy'n berchen ar osodiadau'r defnyddiwr, neu'n rhediad
ail-ddilysedig o'r gorchymyn sydd eisoes yn ysgrifennu'r storfa honno; ni all
dim yma godi braint.

Mae categori na all y system hon ei wasanaethu yn dweud hynny'n blaen ac yn
enwi'r hyn fyddai'n rhaid bodoli. Ni ddangosir rheolydd na fyddai'n newid dim
erioed.

Teipiwch yn y maes chwilio uwchben y bar ochr i'w hidlo i'r categorïau a'r
gosodiadau y mae gair yn eu cyrraedd. Mae `Tab` a `Shift+Tab` yn symud rhwng y
maes chwilio, y llwybr lleoliad, y bar ochr a'r gwarel; mae `Up` a `Down` yn
cerdded y bar ochr ac mae `Enter` yn agor y rhes. Mae `Right` a `Left` yn agor
ac yn cau rhestr y categori o dan y cyrchwr. Mae ffenestr rhy gul yn gollwng y
bar ochr, ac yna mae briwsionyn cyntaf y llwybr yn rhestru'r categorïau.

Caiff ei lansio o res *Settings…* menu system y bwrdd gwaith, neu o gragen
wrth ei lwybr, `/System/Components/settings.app`. Mae'n gofyn am sesiwn
graffigol sy'n rhedeg: heb un mae sianel y ffenestr yn anghyraeddadwy ac mae'n
adrodd y gwrthodiad ar y ffrwd gwall safonol ac yn gorffen.

Mae Sain yn dangos pob allbwn a mewnbwn gyda'i ddewis rhagosodedig, ei lefel
a'i fudo, a'r hyn sy'n recordio: eich recordiadau chi wrth eu henw, a faint
sydd gan bawb arall. Mae ei reolyddion yn newid y ddyfais ar unwaith, a dim
ond tra bo'ch sesiwn yn dal yr ystafell y mae'n ei gwasanaethu; dangosir
dyfais yn ystafell sesiwn arall, ond ni ellir ei newid yma. Mae'ch bwrdd
gwaith yn cofio'r hyn a osodwch. Mae newid lefel sain y ddyfais y mae sain yn
chwarae drwyddi, yma neu o'r bar eiconau, yn chwarae sain fer ar y lefel newydd
— lle daw llusgo i orffwys, ar bob cam o'r bysellau saeth, ac wrth ddad-fudo —
fel y gallwch glywed pa mor uchel yw hi nawr. Yn *Sound Effects*, mae
*Play feedback when the volume changes* yn ei diffodd. Nid yw'n dilyn synau
eraill y bwrdd gwaith: mae'n chwarae ar lefel y ddyfais ei hun, p'un a ydynt
ymlaen ai peidio.

Dim ond pan fo'r peiriant yn un y caiff **Raspberry Pi** ei restru. Mae'n gosod
ffan oeri'r prosesydd, a gaiff ei droi drwy un o linellau'r pennawd 40 pin:
*Off*, *On* drwy'r amser, neu *Automatic*, sy'n troi'r ffan ymlaen pan fydd y
prosesydd yn cyrraedd y tymheredd a osodwch a'i ddiffodd dim ond ar ôl iddo oeri
gan yr ymyl a osodwch, fel nad yw'n troi ymlaen ac i ffwrdd o gwmpas un radd.
Dewiswch y llinell reoli yn ôl ei rhif GPIO a'i phin, a pha un a yw'r ffan yn
rhedeg pan fo'r llinell yn uchel neu'n isel; caiff llinell sy'n cael ei
defnyddio gan rywbeth arall ei henwi yn hytrach na'i chynnig. Ni all llinell o'r
pennawd bweru ffan ar ei phen ei hun, felly cysylltwch y ffan drwy dransistor.
Mae'r paen hefyd yn dangos tymheredd y prosesydd ac a yw'r ffan yn rhedeg. Pan
na all *Automatic* ddarllen y tymheredd, mae'r ffan yn rhedeg nes y gall. Fel
gosodiadau eraill y peiriant, mae newid yn aros am *Apply*, sy'n gofyn am gyfrif
a gaiff eu newid.

Mae **Storage** yn rhestru pob cyfrol sydd wedi'i gosod ac eithrio cyfrol
breifat defnyddiwr arall: y man lle mae wedi'i gosod, ei system ffeiliau, ei
dyfais a'i chyfrwng, pa mor llawn ydyw, ac a yw'n iach. Adroddiad ydyw, nid
rheolydd — gwaith y rheolwr ffeiliau a'r gorchymyn `mount` yw gosod a dad-osod —
ac mae cyfrol nad yw ei fformat yn cadw capasiti sefydlog yn dweud hynny yn
hytrach na dangos bar. Mae `df` yn adrodd yr un ffigurau mewn cragen.

Mae **Files** yn gosod sut mae'r rheolwr ffeiliau'n gweithio: ble mae
ffenestr newydd yn agor a sut mae'n dangos ac yn trefnu'r hyn y mae'n ei
restru, a yw'n dangos colofn y lleoedd a'r bar offer, a yw ffolderi'n aros ar y
brig, pa ôl-ddodiaid enwau a pha eitemau a guddir, a yw llun yn dangos ei
gynnwys ei hun a ffolder yr hyn sydd ynddi, a yw symud eitemau i'r Sbwriel yn
gofyn yn gyntaf, ac a gaiff delwedd ddisg neu archif a agorwyd gennych ei
dad-osod unwaith y caiff ei ffenestri eu cau. Mae'r ôl-ddodiaid enwau y mae'n
eu cuddio wedi'u cuddio ar y bwrdd gwaith hefyd. Mae gan *Show on the desktop*
flwch ar gyfer pob math o yriant — *The system drive*, *Fixed drives*,
*Removable drives*, *Network shares* a *Disk images and archives* — ac mae'r
bwrdd gwaith yn dangos eicon ar gyfer pob gyriant cysylltiedig o fath sydd
wedi'i dicio, sy'n ei agor fel y mae'r rheolwr ffeiliau'n ei wneud; dim ond
gyriannau symudadwy sydd wedi'u ticio i ddechrau. Mae rhes yn dod i rym cyn gynted
ag y caiff ei dewis: mae ble mae ffenestr newydd yn agor ac a yw'n dangos colofn
y lleoedd a'r bar offer yn berthnasol i'r ffenestr nesaf a agorwch, ac mae
popeth arall yn cyrraedd y bwrdd gwaith a phob ffenestr rheolwr ffeiliau ar
unwaith, er bod
ffenestr yn cadw dull dangos neu drefn a ddewisoch chi ynddi eich hun. Mae
*Settings…* ar ddewislen y rheolwr ffeiliau ei hun yn agor y cwarel hwn.

## EXIT STATUS

Sero ar ôl cau glân; nid sero pan wrthodwyd sianel y ffenestr neu'r rhanbarth
fframiau a rennir (nodir y rheswm ar y ffrwd gwall safonol).
