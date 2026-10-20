## NAME

patience — Klondike, FreeCell a Spider, gyda datryswr y tu ôl i bob awgrym

## SYNOPSIS

`patience`

## DESCRIPTION

Yn agor ffenestr ar y bwrdd gwaith wedi'i gosod ar gyfer gêm o amynedd, y gemau
cardiau a elwir hefyd yn solitaire. Mae chwe gêm ar y ddewislen Game, ac mae'r
bysellau `1` i `6` yn eu dewis: Klondike gan godi un cerdyn neu dri, FreeCell, a
Spider gydag un, dau neu bedwar siwt.

Yn Klondike, adeiladwch y saith colofn i lawr mewn lliwiau bob yn ail ac
anfonwch bob siwt adref i'w sylfaen, o'r as i'r brenin. Cliciwch y stoc i droi
cardiau ar y pentwr gwastraff; pan fydd yn wag, cliciwch ei le i droi'r
gwastraff yn ôl. Yn FreeCell mae pob cerdyn yn cael ei ddelio wyneb i fyny ac
mae pob un o'r pedair cell rydd yn dal un cerdyn; mae rhediad yn symud yn un
drwy'r celloedd rhydd a'r colofnau gwag cyhyd ag y bo lle iddo. Yn Spider,
adeiladwch i lawr mewn unrhyw siwt, ond symudwch rediad dim ond pan fydd i gyd
o'r un siwt; mae rhediad cyfan o'r brenin i lawr i'r as yn mynd adref ohono'i
hun. Cliciwch y stoc i ddelio cerdyn ar bob colofn, sy'n gofyn bod gan bob
colofn gerdyn yn gyntaf.

Llusgwch gardiau i ble bynnag y mynnwch, neu cliciwch arnynt: mae clic yn anfon
cardiau i'r lle gorau y gallant fynd iddo, adref yn gyntaf, yna i golofn arall,
yna i golofn wag, yna i gell rydd. Mae cardiau na allant fynd i unman yn neidio
yn eu hunfan. Mae cardiau na allai'r un symudiad diweddarach eu hangen yn mynd
adref ohonynt eu hunain, a chyn gynted ag na all dim atal y gêm rhag cael ei
hennill maen nhw i gyd yn mynd. Gyda Send cards home wedi'i ddiffodd, mae Finish
(`F`) yn eu hanfon.

Daw pob awgrym gan ddatryswr sy'n chwarae'r gêm o'ch blaen, felly mae awgrym
(`H`) yn symudiad sy'n cadw'r gêm yn enilladwy, nid un y mae'r rheolau'n ei
ganiatáu yn unig. Mae'r datryswr yn edrych ar bob safle a gyrhaeddwch. Pan fydd
yn profi nad oes llinell fuddugol ar ôl, mae'r llinell statws yn dweud hynny, ac
mae Back to last winnable (`Ctrl+B`) yn mynd â chi'n ôl i'r safle olaf y gellid
ennill y gêm ohono o hyd.

Gyda Winnable deals only wedi'i droi ymlaen, fel y mae ar y dechrau, mae pob
deliad newydd yn un y mae'r datryswr eisoes wedi'i ennill, ac mae tic wrth rif y
deliad yn dweud hynny. Mae'r datryswr yn rhoi'r gorau i ddeliad na all ei
setlo'n gyflym, ac mae'r deliad a rydd i chi wedyn wedi'i farcio heb ei wirio;
mae Spider gyda phedwar siwt yn aml yn rhy ddwfn i'w setlo mewn pryd. Mae gan
bob deliad ei rif ar y llinell statws, ac mae dewislen y bwrdd ei hun, a agorir
â'r botwm eilaidd, yn chwarae deliad yn ôl ei rif. Mae deliadau FreeCell wedi'u
rhifo fel y mae gemau FreeCell eraill yn eu rhifo, felly mae deliad wedi'i rifo
yr un gêm ym mhobman.

Mae Klondike yn cael ei sgorio yn y ffordd safonol, neu fel Vegas, lle mae pob
gêm yn costio $52 a phob cerdyn a anfonir adref yn ennill $5, gydag un tro
drwy'r stoc wrth godi un cerdyn a thri wrth godi tri, neu fel Vegas cronnus,
sy'n cario'r enillion o gêm i gêm. Mae Spider yn dechrau ar 500 pwynt, yn colli
un am bob symudiad ac yn ennill 100 am bob rhediad a anfonir adref. Ar gyfer pob
gêm mae'r ystadegau'n cadw'r gemau a chwaraewyd ac a enillwyd, y rhediad
presennol o fuddugoliaethau neu golledion, y rhediad hiraf o fuddugoliaethau, y
fuddugoliaeth gyflymaf a'r fuddugoliaeth â'r lleiaf o symudiadau. Mae delio eto
cyn i gêm gael ei hennill yn ei chyfrif yn golled.

Mae'r gêm sydd ar y gweill yn cael ei chadw, felly mae cau'r ffenestr neu adael
yn ei chario drosodd i'r tro nesaf, ac mae'r cloc yn stopio tra bo'r ffenestr
o'r golwg. Mae dewislen y gêm ar y bar eiconau a dewislen y bwrdd ei hun yn dal
y dewisiadau: deliadau enilladwy, y sgorio, anfon cardiau adref, siwtiau pedwar
lliw, cefnau'r cardiau a'r bwrdd.

Mae'r bysellfwrdd yn chwarae popeth y mae'r pwyntydd yn ei chwarae. Mae'r
bysellau saeth yn symud cylch o bentwr i bentwr ac i fyny cardiau colofn; mae
`Space` yn codi'r cardiau ac yn eu gosod lle bynnag y mae'r cylch, mae `Enter`
yn eu hanfon i'w lle gorau ac mae `Escape` yn eu rhoi'n ôl. Mae `D` yn troi'r
stoc, mae `Ctrl+Z` yn dadwneud symudiad ac mae `Ctrl+Y` yn ei wneud eto, mae `N`
yn delio gêm newydd, mae `R` yn delio hon eto ac mae `S` yn dangos yr ystadegau.

Caiff y gêm ei lansio o Lyfrgell Rhaglenni'r bwrdd gwaith, dan Games, neu wrth
ei henw o gragen. Mae angen sesiwn graffigol sy'n rhedeg arni: hebddi mae sianel
y ffenestr yn anghyrraeddadwy ac mae'r gêm yn adrodd y gwrthodiad ar y ffrwd
gwallau safonol ac yn gadael.

## EXIT STATUS

Sero ar ôl gadael yn lân; heb fod yn sero pan wrthodwyd sianel y ffenestr neu'r
rhanbarth ffrâm a rennir (nodir y rheswm ar y ffrwd gwallau safonol).
