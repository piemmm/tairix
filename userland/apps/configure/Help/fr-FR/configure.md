## NAME

configure — lire et régler les paramètres de la machine

## SYNOPSIS

`configure [<key> [<value> [<key> <value>]...]]`

## DESCRIPTION

Liste, affiche et règle les paramètres de la machine, conservés dans
`/System/Settings/Configuration/system.conf`. Sans opérande, chaque
paramètre est listé avec sa valeur actuelle ; avec une clé seule, la
valeur de ce paramètre est affichée ; avec une ou plusieurs paires
`<key> <value>`, ces paramètres sont modifiés ensemble.

Plusieurs paires forment une seule modification, et non plusieurs :
chaque paire est vérifiée avant que quoi que ce soit ne change, puis les
paramètres sont enregistrés en une seule fois. Une ligne de commande qui
nomme une clé inconnue, une valeur hors de l'ensemble de sa clé ou deux
fois la même clé ne change absolument rien ; un groupe de paramètres ne
peut donc jamais rester à moitié appliqué.

Seul un administrateur peut modifier un paramètre : un compte ordinaire
lit les paramètres de la machine mais n'en modifie aucun. Le système
fournit ses propres paramètres, qui restent en vigueur jusqu'à ce qu'un
administrateur en modifie un, et la première modification conserve tous
les autres paramètres fournis par le système. Une modification prend
effet la prochaine fois que démarre la partie du système qui la lit —
`os.loginType` à la prochaine connexion, les commutateurs `cache.*` au
prochain démarrage — sauf mention contraire pour sa clé ci-dessous.

L'ensemble des clés est fermé : une clé inconnue, ou une valeur hors de
l'ensemble d'une clé, est refusée avec l'énoncé des choix valides et ne
change rien.

- `os.loginType` — `text` ou `graphical` : le type de session que le
  service de connexion lance pour un utilisateur authentifié.
  `graphical` (la valeur par défaut) lance directement la session de
  bureau après l'authentification, et se replie sur la connexion texte
  sur une machine qui ne peut en exécuter aucune ; `text` lance le shell
  du compte — le bureau ne s'exécute que depuis une connexion graphique.
- `cache.all` — `on` ou `off` : le commutateur de cache principal. `on`
  (la valeur par défaut) laisse chaque classe de cache ci-dessous
  suivre son propre réglage ; `off` est un plafond qui désactive tout
  cache en mémoire quels que soient les réglages par classe.
- `cache.filesystem`, `cache.block`, `cache.transform`,
  `cache.semantic` — `auto` ou `off` : les commutateurs par classe pour
  les quatre caches mémoire récupérables (les caches du système de
  fichiers, du bloc disque entier, du cluster décompressé et du
  lancement d'applications). `auto` (la valeur par défaut) laisse le
  gestionnaire de pression mémoire gouverner la classe ; `off` la
  désactive entièrement. Il n'y a pas de `on` par classe : une classe
  ne peut pas être forcée à ignorer la pression mémoire. Une classe est
  effectivement `off` dès que `cache.all` est à `off`.

Chaque cache est un accélérateur récupérable, jamais la source de
vérité ; désactiver l'un d'eux ou tous ne fait donc que ralentir le
travail concerné — cela ne change jamais un résultat.

- `net.ipv4.enabled`, `net.ipv6.enabled` — `true` ou `false` : les
  commutateurs de familles d'adresses à l'échelle de la pile. Les deux
  valent `true` par défaut. Une famille désactivée n'attribue aucune
  adresse, ne répond à aucun paquet et refuse un socket de cette
  famille par une erreur typée — jamais un rejet silencieux.
- `net.ipv6.privacy` — `true` ou `false` : si la pile forme des
  adresses IPv6 temporaires (de confidentialité) en plus de l'adresse
  stable. `false` (par défaut) n'utilise que l'adresse SLAAC stable.
- `net.tcp.syncookies` — `auto` ou `always` : la défense contre les
  inondations SYN. `auto` (par défaut) conserve une file semi-ouverte
  bornée et bascule vers des cookies sans état en cas de débordement ;
  `always` répond à chaque demande de connexion sans état. Il n'y a
  pas de `off` — une file de connexions non défendue n'est pas un
  réglage.
- `net.tcp.keepalive` — `true` ou `false` : si les connexions TCP
  envoient des sondes de maintien sur un lien inactif. `false` (par
  défaut) ne sonde jamais et ne ferme jamais une connexion inactive ;
  `true` sonde un pair inactif après l'intervalle habituel et ferme la
  connexion s'il cesse de répondre.
- `net.tcp.ecn` — `true` ou `false` : si les connexions TCP négocient
  la notification explicite de congestion (ECN). `false` (par défaut)
  laisse les connexions Not-ECT ; `true` propose ECN dans la poignée de
  main puis traite une marque de congestion comme un signal de
  ralentissement au lieu de forcer une perte de paquet.
- `net.sockets.mem` — `auto` ou une taille en octets comme `64M` : la
  mémoire que la pile réseau peut détenir en état de sockets pour
  l'ensemble des principaux. `auto` (par défaut) la dimensionne d'après la
  RAM de la machine, pour qu'un grand serveur ne soit pas tenu à un
  chiffre choisi sur une petite ; une taille le remplace pour une charge
  que vous connaissez mieux. Chaque principal peut détenir un seizième
  du budget effectif ; il y a donc toujours de la place pour seize. En
  octets plutôt qu'en nombre de sockets, car le même nombre de sockets
  représente quelques kilo-octets au repos et des mégaoctets une fois
  les tampons pleins : le budget porte beaucoup de connexions calmes ou
  moins de connexions actives, selon la charge réelle.
- `time.servers` — `none` ou une liste de serveurs de temps réseau
  séparés par des virgules, chacun un nom d'hôte ou une adresse. `none`
  (par défaut) ne nomme aucun serveur à vous : l'horloge est alors réglée
  depuis le serveur que nomme le bail d'adresse du réseau, sinon depuis le
  parc public `pool.ntp.org`. Les serveurs nommés ici l'emportent sur les
  deux.
- `time.refresh` — `6h`, `12h`, `1d`, `2d` ou `7d` : le temps de
  fonctionnement écoulé entre deux interrogations de l'horloge une fois
  l'heure connue. `1d` est la valeur par défaut. Une horloge non réglée,
  invraisemblable ou trop ancienne est corrigée dès que le réseau le
  permet, quoi que dise ce réglage.
- `time.network` — `on` (par défaut) ou `off` : si l'horloge est réglée
  depuis le réseau. Avec `off`, aucune requête de temps ne part vers un
  serveur, et l'horloge est réglée à la main ou par la puce d'horloge de
  la carte, lue à chaque démarrage dans tous les cas. Un changement
  s'applique aussitôt ; le réglage est stocké sur le disque système, donc
  la valeur par défaut s'applique en début de démarrage tant que ce disque
  n'est pas atteint.
- `time.zone` — `default` ou le nom d'un fuseau horaire comme
  `Europe/London` : le fuseau horaire que suit la machine, celui
  qu'affiche l'écran de connexion et qu'utilise un compte qui n'en a
  choisi aucun. `default` est le fuseau que le système fournit comme le
  sien ; un nom que la liste des fuseaux du système ne contient pas est
  ignoré, comme s'il valait `default`. Un changement s'applique
  aussitôt.
- `input.mouse.debounce` — en millisecondes entières, `25` par défaut, `0`
  pour désactiver, `100` au maximum : durée après le relâchement d'un bouton
  pendant laquelle l'appui suivant du même bouton est ignoré comme rebond du
  contacteur plutôt que traité comme un nouveau clic. Un contacteur usé peut
  signaler un second appui quelques millisecondes après le relâchement alors
  qu'il n'en visait qu'un. Mettez `0` pour une souris dont le mode tir rapide
  envoie volontairement des paires de clics.
- `audio.output`, `audio.input` — `auto` par défaut, ou l'emplacement d'un
  point de sortie tel que `audioctl` l'affiche (seize chiffres
  hexadécimaux, un point et l'indice du point) : la sortie et l'entrée que
  cette machine préfère par défaut. Le choix propre à une session passe
  avant ; celui-ci vaut avant toute connexion et sur une machine sans
  bureau. `auto` désigne le premier périphérique trouvé.
- `audio.level` — en décibels, `0dB` par défaut, ou une atténuation comme
  `-12dB` ou `-6.5dB`, à deux décimales au plus : le niveau auquel
  démarrent toutes les sorties et entrées. Jamais au-dessus de `0dB`. Le
  service audio prend une valeur modifiée au démarrage suivant ;
  `audioctl` modifie la machine en marche.
- `cooling.fan.mode` — `off` par défaut, `on` ou `auto` : si le ventilateur
  du processeur sur le connecteur GPIO de la carte n'est pas piloté, tourne
  en permanence ou est commandé par la température du processeur.
- `cooling.fan.line` — `none` par défaut, ou la ligne GPIO à laquelle le
  ventilateur est relié, de `0` à `63`. Seule une ligne que le connecteur de
  la carte prête peut être pilotée.
- `cooling.fan.signal` — `active-high` par défaut, ou `active-low` : le
  niveau de la ligne qui fait tourner le ventilateur.
- `cooling.fan.start`, `cooling.fan.hysteresis` — degrés Celsius entiers,
  `65` et `10` par défaut : un ventilateur automatique démarre à `start`, de
  `40` à `85`, et s'arrête une fois que le processeur s'est refroidi de
  `hysteresis`, de `2` à `20`. Un changement d'un réglage du ventilateur
  s'applique aussitôt.

Une clé absente de la liste ci-dessus est cherchée dans un second
registre : les paramètres réseau de chaque interface, conservés dans
`/System/Settings/Network/network.conf`. Ses clés s'écrivent
`<interface>.<setting>`, où `<interface>` est l'alias qu'un
administrateur a donné à une interface réseau (`wan`, `lan0`) et
`<setting>` l'un des noms suivants : `kind`, `match.mac`, `match.node`,
`ipv4.method`, `ipv4.address`, `ipv4.gateway`, `ipv6.method`,
`ipv6.address`, `ipv6.gateway`, `dns.servers`, `mtu`, `bond.members`,
`bond.mode`, `bond.monitor-interval` et `bond.primary`.

Le registre de la machine ci-dessus est toujours consulté en premier ;
un alias d'interface ne peut donc jamais s'approprier le nom d'un
paramètre de la machine.

Les paramètres réseau n'ont pas de valeur par défaut : la liste
n'affiche donc que les paramètres définis, et l'affichage d'un paramètre
non défini répond par une ligne vide plutôt que d'inventer une valeur.
Seul un administrateur peut les lire ou les modifier : ils désignent le
matériel de chaque interface et les adresses de cette machine. Pour tout
autre compte, la liste affiche les paramètres de la machine et signale
que ceux du réseau ne sont pas affichés.

Comme ce registre n'a pas de valeur par défaut, un paramètre est
supprimé plutôt que réinitialisé, et c'est une **valeur vide** qui le
supprime : `configure wan.ipv4.address ""`. Aucun paramètre d'interface
n'accepte de valeur vide ; cette écriture ne peut donc jamais être
confondue avec le réglage d'une valeur. C'est ce qui permet à une seule
commande de faire passer une interface d'une adresse statique à DHCP —
`configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
— une modification dont aucune des deux moitiés, prise seule, ne décrit
une configuration cohérente. Une interface dont le dernier paramètre est
supprimé n'est plus déclarée du tout.

Toute la configuration modifiée est vérifiée avant le moindre
enregistrement : une modification qui la laisserait incohérente (une
méthode statique sans adresse, un agrégat de liens de moins de deux
membres) est refusée et rien n'est enregistré.

Modifier un paramètre `net.*`, ou un paramètre d'interface, l'enregistre
et le transmet à la pile réseau en marche ; il prend donc effet
aussitôt. Seules les interfaces que la modification touche réellement
sont transmises. Si la pile en marche n'accepte pas une transmission —
aucune ne tourne, ou votre compte ne peut pas administrer le réseau —,
le paramètre est tout de même enregistré et `configure` le signale ; il
s'applique alors au prochain démarrage.

Deux limites sont signalées plutôt que cachées. Une interface que vous
avez retirée de la configuration continue de fonctionner jusqu'au
prochain démarrage avec la dernière configuration transmise à la pile,
car la pile n'a aucun message qui retire une interface. Une interface
enregistrée sans `match.mac` ni `match.node` ne peut jamais être liée à
un périphérique ; elle est donc enregistrée, et le refus est énoncé.

## OPTIONS

- `-h, -?` — afficher l'aide courte de cette commande.

## EXAMPLES

- `configure` — lister tous les paramètres.
- `configure os.loginType` — afficher le type de session par défaut.
- `configure os.loginType graphical` — démarrer sur la connexion
  graphique.
- `configure cache.all off` — désactiver tout cache en mémoire sur tout
  le système.
- `configure cache.filesystem off` — désactiver uniquement le cache du
  système de fichiers.
- `configure net.ipv6.enabled false` — désactiver IPv6 dans toute la
  pile.
- `configure time.servers 0.example.test,1.example.test` — définir les
  serveurs de temps réseau sur lesquels l'horloge se synchronise.
- `configure time.zone Europe/London` — mettre la machine à l'heure de
  Londres.
- `configure wan.ipv4.address` — afficher l'adresse IPv4 statique
  configurée pour l'interface nommée `wan`.
- `configure wan.ipv4.address 10.0.0.7/24 wan.ipv4.gateway 10.0.0.1` —
  donner à `wan` une adresse IPv4 statique et une passerelle par défaut.
- `configure wan.ipv4.method dhcp wan.ipv4.address "" wan.ipv4.gateway ""`
  — faire passer `wan` de l'adressage statique à DHCP.
- `configure wan.dns.servers 9.9.9.9,2001:db8::53` — définir les
  serveurs DNS récursifs à utiliser sur `wan`.

## EXIT STATUS

- `0` — la liste, la valeur, l'aide courte ou la modification a été
  effectuée.
- `1` — les paramètres n'ont pas pu être lus ou modifiés (par exemple
  l'appelant ne peut pas modifier les paramètres de la machine, ou ne
  peut pas lire ses paramètres réseau), ou la sortie n'a pas pu être
  délivrée.
- `2` — la ligne de commande n'a pas été comprise, la clé est inconnue,
  la valeur est hors de l'ensemble de la clé, ou la modification
  laisserait la configuration réseau incohérente.

## ENVIRONMENT

- `LANG` — la langue préférée de l'aide courte (une étiquette BCP-47
  comme `fr-FR`).

## SEE ALSO

- `man`
