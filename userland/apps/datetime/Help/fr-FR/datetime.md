## NAME

datetime — date, heure et fuseau horaire de la machine

## SYNOPSIS

`datetime`

## DESCRIPTION

Ouvre la fenêtre **Date & Time** (Date et heure) : ce qu'indique
l'horloge de la machine, d'où vient cette heure et les contrôles qui la
règlent. On y accède depuis le menu propre à l'horloge du bureau —
faites un clic droit sur l'horloge dans la barre d'icônes et choisissez
**Set Date & Time…** (Régler la date et l'heure…) — et, avant qu'elle ne
s'ouvre, le bureau demande le mot de passe d'un compte autorisé à
modifier ces réglages.

Le haut de la fenêtre affiche l'heure à la seconde près, la date et le
fuseau horaire avec son abréviation et son décalage par rapport à UTC,
le tout dans le fuseau propre à la machine.

**Date and Time** (Date et heure) contient l'horloge elle-même.

- **Set date and time automatically** (Régler la date et l'heure
  automatiquement) maintient l'horloge à l'heure grâce aux serveurs de
  temps du réseau. En dessous, la fenêtre indique d'où vient l'heure de
  l'horloge : réglée depuis le réseau, et quand ; réglée à la main ; lue
  sur la puce d'horloge de la carte au démarrage de la machine ; ou pas
  réglée du tout. Activer ou désactiver l'interrupteur prend effet
  aussitôt.
- Tant que l'interrupteur est activé, le calendrier et l'heure sont
  affichés mais ne peuvent pas être modifiés. Désactivez-le pour régler
  l'horloge à la main.
- Choisissez un jour dans le calendrier, saisissez l'heure ou ajustez-la
  pas à pas, puis appuyez sur **Set**. Les touches fléchées déplacent le
  jour choisi d'un jour ou d'une semaine, `Page Up` et `Page Down` d'un
  mois, et avec `Shift` d'un an ; les boutons à côté du mois et de
  l'année font défiler les mois et les années sans changer le jour. Dans
  la saisie de l'heure, `Tab` ou `:` passe des heures aux minutes puis
  aux secondes, et `Up` et `Down` font avancer ou reculer d'un cran la
  partie où se trouve le curseur. Au format douze heures, la dernière
  partie est **AM** ou **PM** : cliquez dessus, ou appuyez sur `A` ou
  `P`. `Enter` règle l'horloge.
- Tant que vous n'avez ni choisi de jour ni modifié l'heure, le
  calendrier et l'heure suivent la marche de l'horloge. Une fois que vous
  l'avez fait, ils conservent votre choix jusqu'à ce que vous régliez
  l'horloge ou fermiez la fenêtre.

La date et l'heure que vous réglez s'entendent dans le fuseau horaire de
la machine. Une heure que les horloges sautent lorsqu'elles avancent est
refusée, et la fenêtre indique quand a lieu le changement d'heure. Pour
une heure que les horloges parcourent deux fois lorsqu'elles reculent,
la fenêtre vous pose la question : indiquez si vous parlez de celle
d'avant le changement ou de celle d'après. Une date antérieure à la
création de ce système, ou postérieure de plus d'un siècle à celle-ci,
n'est réglée qu'après votre confirmation, car les certificats ne seraient
pas reconnus comme fiables et les fichiers paraîtraient plus anciens, ou
plus récents, qu'ils ne le sont.

Une fois l'horloge réglée, la puce d'horloge de la carte est mise à la
même heure, afin que la machine la conserve lors d'un redémarrage. Si la
machine n'a pas de puce d'horloge, la fenêtre prévient que l'heure sera
perdue au prochain redémarrage, à moins que l'heure réseau ne soit
activée.

**Time Zone** (Fuseau horaire) nomme le fuseau que suit la machine,
indique quand ses horloges avanceront ou reculeront la prochaine fois et
de combien, et propose le bouton **Change…**, qui ouvre la liste de tous
les lieux que le système connaît : tapez une partie du nom d'une ville,
d'une région ou d'un fuseau pour la restreindre, puis choisissez avec
`Enter` ou un double-clic. Chaque lieu indique l'heure qu'il y est en ce
moment. Un nouveau fuseau prend effet aussitôt.

Le fuseau de la machine est celui qu'affiche l'écran de connexion, et
celui qu'utilise tout compte qui n'a pas choisi son propre fuseau dans
**Settings** (Réglages).

Si le compte sous lequel la fenêtre s'exécute n'a pas le droit
d'effectuer une modification, celle-ci est refusée, une alerte en donne
la raison et la fenêtre montre ce qui est réellement en vigueur.
`Escape` ou **Close** ferme la fenêtre.

## EXIT STATUS

Zéro lorsque la fenêtre est fermée, y compris après une modification
refusée. Non nul lorsque la fenêtre n'a pas pu être ouverte ou que sa
connexion au bureau a été perdue ; la raison est indiquée sur le flux
d'erreur standard.

## SEE ALSO

`configure`
