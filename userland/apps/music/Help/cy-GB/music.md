## NAME

music — chwaraewr cerddoriaeth y bwrdd gwaith

## SYNOPSIS

`music`

## DESCRIPTION

Mae'n chwarae rhestr o draciau mewn un ffenestr. Agorwch ffeiliau neu ffolder
cyfan drwy ddewisydd ffeiliau'r bwrdd gwaith, neu agorwch drac o'r rheolwr
ffeiliau: mae'n ymuno â rhestr y chwaraewr sydd eisoes ar agor. Mae traciau o'r
un gyfradd, yr un fformat sampl a'r un gosodiad sianeli yn chwarae'n syth i'w
gilydd heb fwlch.

Nid oes gan y chwaraewr unrhyw allu dros y system ffeiliau. Mae sesiwn y bwrdd
gwaith yn pori ar ei ran ac yn dirprwyo iddo, unwaith ac i'w darllen yn unig,
yr union ffeiliau y mae'r defnyddiwr yn eu dewis — ar gyfer ffolder, y ffeiliau
ynddi y mae'r chwaraewr hwn yn eu hagor. Nid oes ffeil yn cael ei datgodio y tu
mewn i'r chwaraewr: datgodir ei sain a'i chlawr albwm gan broses ar wahân heb
unrhyw gyrhaeddiad, fel na all ffeil wallus neu elyniaethus gyrraedd dim y gall
y chwaraewr ei gyrraedd.

Mae brig y ffenestr yn dangos beth sy'n chwarae: clawr yr albwm, y teitl, yr
artist a'r albwm, y fformat, y man yn y trac, a mesurydd lefel ar gyfer pob
sianel. Odanynt mae'r rheolyddion chwarae, y rheolyddion cymysgu ac ailadrodd
a'r sain, ac odanynt hwy y rhestr. Llusgwch y llithrydd safle i neidio a'r
llithrydd sain i osod y lefel; mae'r ddau'n gweithredu lle rydych yn eu
gollwng. Cliciwch ddwywaith ar drac i'w chwarae. Mae gwasgiad eilaidd ar y
rhestr yn agor dewislen y chwaraewr, sydd hefyd yn dewis yr allbwn ac a yw
traciau'n cael eu lefelu yn ôl y cryfder y mae eu tagiau eu hunain yn ei
nodi.

Gadewir allan ffeil na all y chwaraewr ei darllen, gyda'r rheswm ar y llinell
statws.

* `Space` — chwarae neu oedi
* `Enter` — chwarae'r trac a ddewiswyd
* `Left` / `Right` — deg eiliad yn ôl neu ymlaen
* `Ctrl` + `Left` / `Right` — y trac blaenorol neu'r nesaf
* `Up` / `Down` — dewis y trac uwchben neu islaw
* `Alt` + `Up` / `Down` — symud y trac a ddewiswyd
* `Delete` — tynnu'r trac a ddewiswyd oddi ar y rhestr
* `+` / `-` — tri desibel yn uwch neu'n is
* `S` — cymysgu neu chwarae mewn trefn
* `R` — ailadrodd dim, y rhestr, neu'r trac
* `Ctrl` + `O` — agor ffeiliau
* `Ctrl` + `Shift` + `O` — agor ffolder

## OPTIONS

`-h`, `-?`, `--help`
: Ysgrifennu'r cymorth hwn i'r allbwn safonol a gadael.

## EXIT STATUS

Sero ar ôl cau'r ffenestr neu ddewis Gadael. Nid sero pan wrthodwyd sianel y
ffenestr, y rhanbarth ffrâm a rennir, neu sesiwn y bwrdd gwaith; nodir y
rheswm ar y ffrwd gwall safonol.
