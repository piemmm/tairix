## NAME

mv — déplacer (renommer) des fichiers et des répertoires

## SYNOPSIS

`mv [-finvT] [-t dir] [--] source... dest`

## DESCRIPTION

Déplace chaque opérande source vers une destination. Avec une seule
source et une destination qui ne nomme pas un répertoire, la source
est renommée vers ce chemin exact. Quand la destination nomme un
répertoire existant — et toujours quand il y a plus d'une source —
chaque source est déplacée *dans* ce répertoire sous son propre nom
de base.

Un déplacement au sein d'un volume est un renommage atomique qui
préserve l'identité du nœud. Un déplacement entre volumes tient la même
promesse : la copie est construite à côté de la destination, écrite sur
le disque et renommée à sa place en une seule étape, et la source n'est
supprimée qu'ensuite — la destination garde donc ce qu'elle contenait
tant que le déplacement n'est pas complet, et ni un échec ni un plantage
ne perd quoi que ce soit. Dans les deux cas, un répertoire ne remplace
qu'un répertoire vide, rien d'autre ne remplace un répertoire, et aucun
répertoire n'est déplacé dans sa propre sous-arborescence.

Une destination existante est écrasée par défaut, ignorée avec `-n`,
et fait l'objet d'une question sur la sortie d'erreur avec `-i` (une
question déclinée ignore ce déplacement sans erreur ; une réponse
illisible ne vaut jamais consentement). Le premier échec arrête
l'exécution avant tout opérande ultérieur. `--` termine l'analyse des
options : chaque argument ultérieur est un chemin.

La copie conserve les trous de chaque fichier, ses attributs étendus —
un type de fichier RISC OS, un type et un créateur Mac —, ses droits
d'accès et son propriétaire partout où la destination les garde, et
déplace un lien symbolique comme un lien de même cible, jamais ce qu'il
désigne. Un fichier que l'auteur du déplacement ne peut pas rendre à son
propriétaire devient le sien, sans ses bits set-ID. Les horodatages ne
sont pas conservés : aucun appel de ce système ne les fixe. Les
attributs que la destination ne peut pas garder sont signalés dans un
seul avertissement à la fin de l'exécution, et dans un enregistrement
`omission` sur le flux d'information standard, avant que les sources ne
disparaissent pour de bon.

## OPTIONS

- `-f, --force` — ne jamais demander avant d'écraser. Le dernier de
  `-f`, `-i` et `-n` l'emporte.
- `-i, --interactive` — demander avant d'écraser une destination
  existante ; seule une réponse commençant par `y`/`Y` consent.
- `-n, --no-clobber` — ne jamais écraser une destination existante.
- `-v, --verbose` — signaler chaque déplacement par
  `renamed 'source' -> 'dest'`, et un déplacement entre volumes par
  `copied 'source' -> 'dest'` puis `removed 'source'`.
- `-t dir, --target-directory=dir` — déplacer chaque source dans
  `dir`, qui doit être un répertoire existant. La valeur suit
  attachée (`-tdir`, `--target-directory=dir`) ou comme argument
  suivant.
- `-T, --no-target-directory` — traiter la destination comme un
  fichier ordinaire ; exactement une source est permise. Incompatible
  avec `-t`.
- `-h, -?, --help` — afficher l'aide courte de cette commande.

## EXAMPLES

- `mv draft.txt final.txt` — renommer un fichier.
- `mv -v a.txt b.txt Archive` — déplacer les deux fichiers dans
  `Archive`, en signalant chaque déplacement.
- `mv -n new.cfg current.cfg` — installer un fichier seulement si la
  destination n'existe pas déjà.

## EXIT STATUS

- `0` — chaque déplacement a réussi (un saut `-n` et une question
  `-i` déclinée ne sont pas des échecs).
- `1` — un échec du système de fichiers, de la question ou de la
  sortie ; la raison est imprimée sur la sortie d'erreur.
- `2` — la ligne de commande n'a pas été comprise.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette
  BCP-47 telle que `fr-FR`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
