## NAME

audioctl — lister les périphériques audio et modifier leurs réglages

## SYNOPSIS

`audioctl [devices]`

`audioctl streams [-a]`

`audioctl default <device>`

`audioctl level <device> <level>`

`audioctl mute <device>`

`audioctl unmute <device>`

## DESCRIPTION

Liste les sorties et les entrées que voit cette session : l'identifiant de
chacune pour ce démarrage, si elle est la valeur par défaut de son sens, son
niveau et sa sourdine, la fréquence à laquelle elle tourne, les trames
perdues, son emplacement et son nom. `streams` liste vos propres flux, et avec
`--all` ceux de tous les principaux.

`default`, `level`, `mute` et `unmute` modifient les réglages d'un
périphérique. Un périphérique est nommé par une référence `audio:` :
`audio:sink/default` ou `audio:source/default` pour celui par défaut à cet
instant, `audio:sink/<id>` pour ce démarrage, ou `audio:sink/<location>` où
que soit le périphérique, tels que la liste les nomme. Un niveau s'exprime en
décibels au centième, 0 ou moins, par exemple `-6` ou `-3.5` ; un niveau
négatif n'exige pas de `--`.

Les réglages d'un périphérique appartiennent à la salle qu'il dessert. La
session qui tient cette salle peut les modifier, n'importe qui le peut tant
que la salle n'est pas prise, et personne tant qu'elle est retenue ; un refus
le dit. Ce qu'une session règle lui appartient : pendant qu'une autre session
tient la salle, ses réglages s'effacent, et ils reviennent à son retour. Le
niveau de départ de chaque périphérique et les périphériques préférés par
défaut sont les réglages de la machine `audio.level`, `audio.output` et
`audio.input`, que `configure` définit.

Sur l'information standard (fd 3), `audioctl streams` écrit un enregistrement
`omission` lorsqu'il ne liste que vos propres flux.

## OPTIONS

- `-a, --all` — avec `streams`, les flux de tous les principaux ; il faut `CAP_SYSINFO_GLOBAL`.
- `-h, -?, --help` — afficher l'aide courte de cette commande.
- `--version` — afficher la version, puis quitter.

## EXAMPLES

- `audioctl` — lister les sorties et les entrées.
- `audioctl level audio:sink/default -10` — régler la sortie par défaut à dix décibels sous le maximum.
- `audioctl mute audio:source/default` — couper l'entrée par défaut.
- `audioctl default audio:sink/2` — faire de la sortie 2 la sortie par défaut.
- `audioctl streams --all` — lister les flux de tous les principaux.

## EXIT STATUS

- `0` — la commande a abouti.
- `1` — elle a été refusée, ou n'a pas pu être exécutée.
- `2` — la ligne de commande n'a pas été comprise.

## ENVIRONMENT

- `LANG` — la langue préférée pour l'aide courte (une étiquette BCP-47 comme `fr-FR`).

## SEE ALSO

- `play`
- `configure`
- `sysinfo`
- `man`
