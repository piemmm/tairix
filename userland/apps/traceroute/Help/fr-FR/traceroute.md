## NAME

traceroute — afficher le chemin que suivent les sondes d'écho jusqu'à un hôte du réseau

## SYNOPSIS

`traceroute [option...] host [packetlen]`

## DESCRIPTION

Envoie des requêtes d'écho ICMP (IPv4) ou ICMPv6 (IPv6) vers un hôte avec
une limite de sauts qui monte d'un saut à la fois, et affiche chaque
routeur qui signale l'épuisement de la limite de sauts d'une sonde,
jusqu'à ce que l'hôte lui-même réponde. Les sondes passent par une socket
d'écho ICMP ouverte dans la pile réseau en espace utilisateur, soumise à
`CAP_NET` et `CAP_NET_RAW` et auditée.

La cible est une adresse IPv4 ou IPv6 littérale ou un nom d'hôte, résolu
par le résolveur stub du système ; une adresse littérale ne demande aucune
requête.

Chaque saut s'affiche sur sa propre ligne une fois toutes ses sondes
tranchées : son numéro, puis pour chaque sonde le temps d'aller-retour,
précédé de l'adresse — et, sans `-n`, du nom — de celui qui a répondu
chaque fois qu'il diffère de la sonde précédente. Une sonde restée sans
réponse dans le délai d'attente s'affiche `*`. Un routeur qui signale que
le chemin ne peut continuer marque sa sonde — `!N` réseau injoignable,
`!H` hôte injoignable, `!P` protocole injoignable, `!X` interdit
administrativement, `!S` échec de la route source, `!F-mtu` paquet trop
grand, `!<code>` tout autre — et le tracé s'arrête à ce saut. Il s'arrête
aussi dès que l'hôte répond, ou à la borne de sauts.

Plusieurs sondes sont en attente à la fois (`-N`), si bien qu'une suite de
sauts muets coûte une attente au lieu d'une par sonde ; un saut n'est
toutefois affiché qu'une fois chaque saut précédent affiché.

Les sondes sont des requêtes d'écho ICMP, la méthode que choisit `-I` ;
une option choisissant des sondes UDP ou TCP est refusée plutôt qu'ignorée
en silence.

## OPTIONS

- `-4` — exiger une cible IPv4.
- `-6` — exiger une cible IPv6.
- `-I, --icmp` — sonder par écho ICMP (la seule méthode, et celle par
  défaut).
- `-f, --first` — la limite de sauts des premières sondes (par défaut 1).
- `-m, --max-hops` — la plus haute limite de sauts sondée (par défaut 30).
- `-q, --queries` — sondes envoyées par saut (par défaut 3, au plus 10).
- `-N, --sim-queries` — sondes en attente à la fois (par défaut 16, au
  plus 64).
- `-w, --wait` — secondes d'attente de la réponse de chaque sonde (un
  décimal, par défaut 5).
- `-n` — sortie numérique : adresses seules, sans résolution inverse.
- `-?, --help` — afficher l'aide courte de cette commande.

`packetlen` est la longueur totale de chaque sonde en octets, en-têtes
compris (par défaut 60 pour IPv4 et 80 pour IPv6).

## EXAMPLES

- `traceroute example.com` — tracer le chemin jusqu'à un hôte.
- `traceroute -n 192.0.2.1` — sortie numérique, sans recherche de noms.
- `traceroute -q 1 -w 1 -m 20 gateway` — une sonde rapide par saut, au
  plus vingt sauts.
- `traceroute -6 host.example` — tracer en IPv6.

## EXIT STATUS

- `0` — le tracé s'est déroulé, qu'il ait atteint l'hôte ou non (ou l'aide
  courte a été écrite).
- `2` — la ligne de commande n'a pas été comprise, la cible ne s'est pas
  résolue, la socket n'a pas pu être ouverte, ou une sonde n'a pas pu être
  envoyée.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`).

## SEE ALSO

- `ping`
- `host`
- `ss`
- `man`
