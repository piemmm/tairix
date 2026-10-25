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
promesse : la copie est construite à côté de la destination, lisible par
son seul propriétaire tant qu'elle n'est pas complète, écrite sur le
disque et renommée à sa place en une seule étape, et ce qui a été copié
n'est supprimé qu'ensuite — la destination garde donc ce qu'elle
contenait tant que le déplacement n'est pas complet, et ni un échec ni
un plantage ne perd quoi que ce soit. Un fichier ajouté à la source, ou
modifié, pendant sa copie est conservé là où il était et signalé, ainsi
que le répertoire qui le contient ; sur un volume qui ne tient aucun
compteur de modifications, ou qui ne garde que des horodatages grossiers
(deux secondes pour FAT), une réécriture de même longueur faite dans cet
intervalle peut passer inaperçue. Dans les deux cas, un répertoire ne
remplace qu'un répertoire vide, rien d'autre ne remplace un répertoire,
et aucun répertoire n'est déplacé dans sa propre sous-arborescence.

Une destination existante est écrasée par défaut, conservée avec `-n`
(qui signale `not replacing` et fait échouer l'exécution, comme chez
GNU), et fait l'objet d'une question sur la sortie d'erreur avec `-i`
(une question déclinée ignore ce déplacement et fait échouer
l'exécution ; une réponse illisible ne vaut jamais consentement). Un
opérande qui ne peut pas être déplacé est signalé, et le suivant est
tout de même déplacé. `--` termine l'analyse des options : chaque
argument ultérieur est un chemin.

La copie conserve les trous de chaque fichier, ses attributs étendus —
un type de fichier RISC OS, un type et un créateur Mac —, ses droits
d'accès et son propriétaire partout où la destination les garde, et
déplace un lien symbolique comme un lien de même cible, jamais ce qu'il
désigne. Un fichier rendu à un autre propriétaire arrive sans ses bits
set-ID, que le système efface quand le propriétaire change ; un fichier
que l'auteur du déplacement ne peut pas rendre devient le sien, dans son
propre groupe si l'auteur du déplacement peut l'y laisser, et sans les
bits set-ID d'un propriétaire ou d'un groupe non conservé. Les
horodatages ne sont pas conservés : aucun appel de ce système ne les
fixe. Les attributs que la destination ne peut pas garder sont signalés
dans un seul avertissement à la fin de l'exécution, et dans un
enregistrement `omission` sur le flux d'information standard, avant que
les sources ne disparaissent pour de bon.

## OPTIONS

- `-f, --force` — ne jamais demander avant d'écraser. Le dernier de
  `-f`, `-i` et `-n` l'emporte.
- `-i, --interactive` — demander avant d'écraser une destination
  existante ; seule une réponse commençant par `y`/`Y` consent.
- `-n, --no-clobber` — ne jamais écraser une destination existante ;
  chaque destination conservée est signalée par `not replacing 'dest'`.
- `-v, --verbose` — signaler chaque déplacement par
  `renamed 'source' -> 'dest'`, et un déplacement entre volumes entrée
  par entrée, comme le fait GNU : `created directory 'dest'` pour chaque
  répertoire et `copied 'source' -> 'dest'` pour tout le reste, au fur
  et à mesure de la copie, puis `removed 'source'` ou
  `removed directory 'source'` pour chacun, à mesure que la source
  disparaît.
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
  destination n'existe pas déjà (en signalant `not replacing` si elle
  existe).

## EXIT STATUS

- `0` — chaque déplacement a réussi.
- `1` — un opérande n'a pas pu être déplacé, `-n` a conservé une
  destination, une question `-i` a été déclinée, quelque chose dans une
  source a changé pendant son déplacement et a été conservé, ou une
  question ou la sortie a échoué ; la raison de chaque échec est
  imprimée sur la sortie d'erreur. Une ligne de commande non comprise
  sort aussi avec `1`.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette
  BCP-47 telle que `fr-FR`).

## SEE ALSO

- `cp`
- `ls`
- `rm`
