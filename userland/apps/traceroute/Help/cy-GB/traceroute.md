## NAME

traceroute — argraffu'r llwybr y mae chwilwyr adlais yn ei gymryd at westeiwr rhwydwaith

## SYNOPSIS

`traceroute [option...] host [packetlen]`

## DESCRIPTION

Mae'n anfon ceisiadau adlais ICMP (IPv4) neu ICMPv6 (IPv6) tuag at
westeiwr gyda therfyn hopian sy'n codi un hop ar y tro, ac yn argraffu pob
llwybrydd sy'n adrodd bod terfyn hopian chwiliwr wedi dod i ben, nes bod y
gwesteiwr ei hun yn ateb. Mae'r chwilwyr yn mynd drwy soced adlais ICMP a
agorir yn y pentwr rhwydwaith yng ngofod y defnyddiwr, wedi'i reoli gan
`CAP_NET` a `CAP_NET_RAW` ac wedi'i archwilio.

Y targed yw cyfeiriad IPv4 neu IPv6 llythrennol neu enw gwesteiwr, wedi'i
ddatrys drwy ddatryswr stwbyn y system; nid oes angen ymholiad ar
gyfeiriad llythrennol.

Mae pob hop yn argraffu ar ei linell ei hun unwaith y bydd ei holl
chwilwyr wedi setlo: ei rif, yna ar gyfer pob chwiliwr yr amser mynd a
dod, gyda chyfeiriad — ac, oni roddir `-n`, enw — yr un a atebodd o'i
flaen pryd bynnag y mae'n wahanol i'r chwiliwr cynt. Mae chwiliwr heb ateb
o fewn yr aros yn argraffu `*`. Mae llwybrydd sy'n adrodd na all y llwybr
fynd ymlaen yn marcio'i chwiliwr — `!N` rhwydwaith yn anghyraeddadwy, `!H`
gwesteiwr yn anghyraeddadwy, `!P` protocol yn anghyraeddadwy, `!X` wedi'i
wahardd yn weinyddol, `!S` llwybr ffynhonnell wedi methu, `!F-mtu` pecyn
yn rhy fawr, `!<code>` unrhyw un arall — ac mae'r olrhain yn stopio wrth
yr hop hwnnw. Mae hefyd yn stopio unwaith y bydd y gwesteiwr yn ateb, neu
wrth y terfyn hopian.

Mae sawl chwiliwr yn aros ar yr un pryd (`-N`), felly mae cyfres o hopiau
mud yn costio un aros yn hytrach nag un fesul chwiliwr; ond nid yw hop yn
cael ei argraffu nes bod pob hop o'i flaen wedi'i argraffu.

Ceisiadau adlais ICMP yw'r chwilwyr, y dull y mae `-I` yn ei ddewis; mae
opsiwn sy'n dewis chwilwyr UDP neu TCP yn cael ei wrthod yn hytrach na'i
anwybyddu'n dawel.

## OPTIONS

- `-4` — mynnu targed IPv4.
- `-6` — mynnu targed IPv6.
- `-I, --icmp` — chwilio ag adlais ICMP (yr unig ddull, a'r rhagosodiad).
- `-f, --first` — y terfyn hopian sydd gan y chwilwyr cyntaf (rhagosodiad
  1).
- `-m, --max-hops` — y terfyn hopian uchaf a chwilir (rhagosodiad 30).
- `-q, --queries` — chwilwyr a anfonir fesul hop (rhagosodiad 3, 10 ar y
  mwyaf).
- `-N, --sim-queries` — chwilwyr yn aros ar yr un pryd (rhagosodiad 16, 64
  ar y mwyaf).
- `-w, --wait` — eiliadau i aros am ateb pob chwiliwr (degolyn,
  rhagosodiad 5).
- `-n` — allbwn rhifol: cyfeiriadau yn unig, heb chwilio o chwith.
- `-?, --help` — dangos cymorth byr y gorchymyn hwn ei hun.

`packetlen` yw cyfanswm hyd pob chwiliwr mewn beitiau, gan gynnwys y
penynnau (rhagosodiad 60 ar gyfer IPv4 ac 80 ar gyfer IPv6).

## EXAMPLES

- `traceroute example.com` — olrhain y llwybr at westeiwr.
- `traceroute -n 192.0.2.1` — allbwn rhifol, dim chwilio enwau.
- `traceroute -q 1 -w 1 -m 20 gateway` — un chwiliwr cyflym fesul hop,
  ugain hop ar y mwyaf.
- `traceroute -6 host.example` — olrhain dros IPv6.

## EXIT STATUS

- `0` — rhedodd yr olrhain, p'un a gyrhaeddodd y gwesteiwr ai peidio (neu
  ysgrifennwyd y cymorth byr).
- `2` — ni ddeallwyd y llinell orchymyn, ni ddatryswyd y targed, ni ellid
  agor y soced, neu ni ellid anfon chwiliwr.

## ENVIRONMENT

- `LANG` — y locale a ffefrir ar gyfer y cymorth byr (tag BCP-47 fel
  `fr-FR`).

## SEE ALSO

- `ping`
- `host`
- `ss`
- `man`
