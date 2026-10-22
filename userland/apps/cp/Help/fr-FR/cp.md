## NAME

cp — copier des fichiers et des répertoires

## SYNOPSIS

`cp [-dfilnPrRsvT] [--sparse=WHEN] [--preserve=LIST] [--no-preserve=LIST] [-t dir] [--] source... dest`

## DESCRIPTION

Copie chaque opérande source vers une destination. Avec une seule
source et une destination qui ne nomme pas un répertoire, la source
est copiée vers ce chemin exact. Quand la destination nomme un
répertoire existant — et toujours quand il y a plus d'une source —
chaque source est copiée *dans* ce répertoire sous son propre nom de
base.

Une source répertoire n'est copiée qu'avec `-r`, qui reproduit tout le
sous-arbre ; sans `-r` un opérande répertoire est refusé. Un fichier
de destination existant est écrasé par défaut, ignoré avec `-n`, et
fait l'objet d'une question sur la sortie d'erreur avec `-i` (une
question déclinée ignore cette copie sans erreur ; une réponse
illisible ne vaut jamais consentement).
Une destination qui est le fichier source lui-même — le même nom, un
autre nom de ce fichier ou un lien vers lui — est refusée comme étant
le même fichier et reste telle qu'elle était.

Le premier échec arrête l'exécution avant tout opérande ultérieur.
`--` termine l'analyse des options : chaque argument ultérieur est un
chemin.

Chaque copie emporte les attributs étendus de la source — un type de
fichier RISC OS, le type et le créateur d'un Mac — partout où la destination
peut les conserver, et garde comme trous les trous d'un fichier creux. Les
attributs qu'une destination ne peut pas conserver sont signalés par un seul
avertissement à la fin de l'exécution, et par un enregistrement `omission`
sur le flux d'information standard ; avec `--preserve=xattr`, une telle
copie échoue à la place.

## OPTIONS

- `-r, -R, --recursive` — copier les répertoires et leur contenu.
- `-f, --force` — quand un fichier de destination ne peut pas être
  créé, le supprimer et réessayer la copie une fois.
- `-i, --interactive` — demander avant d'écraser un fichier existant ;
  seule une réponse commençant par `y`/`Y` consent.
- `-n, --no-clobber` — ne jamais écraser un fichier existant. Le
  dernier de `-i` et `-n` l'emporte.
- `-l, --link` — donner à la destination un second nom du nœud de la
  source au lieu de copier ses octets, de sorte que les deux noms ne
  puissent diverger à une écriture ultérieure. Un répertoire source exige
  toujours `-r`.
- `-s, --symbolic-link` — créer un lien symbolique désignant la source au
  lieu de la copier.
- `-P, --no-dereference` — reproduire un lien symbolique source comme un
  lien stockant la même cible, mot pour mot, au lieu de copier ce qu'il
  désigne (ainsi un lien relatif ou cassé survit à la copie). Sans cette
  option, un lien source est suivi.
- `--preserve=links` — deux sources désignant un même nœud obtiennent
  deux noms à la destination plutôt que deux copies, de sorte que la
  copie ne double pas silencieusement le stockage.
- `--preserve=xattr` — chaque attribut étendu doit être emporté : une copie
  dont la destination ne peut en conserver un échoue en le nommant.
  `--preserve=links,xattr` demande les deux.
- `--no-preserve=xattr` — ne copier aucun attribut étendu. `mode`,
  `ownership` et `timestamps` sont aussi acceptés, et déjà non préservés.
- `--sparse=WHEN` — la manière de copier un fichier creux : `auto`, la
  valeur par défaut, garde ses trous ; `always` laisse aussi non écrit chaque
  bloc de zéros qu'il stocke ; `never` écrit chaque octet.
- `-d` — `-P` et `--preserve=links` ensemble, comme dans l'outil GNU.
- `-v, --verbose` — signaler chaque copie sous la forme
  `'source' -> 'dest'`.
- `-t dir, --target-directory=dir` — copier chaque source dans `dir`,
  qui doit être un répertoire existant. La valeur suit attachée
  (`-tdir`, `--target-directory=dir`) ou comme argument suivant.
- `-T, --no-target-directory` — traiter la destination comme un
  fichier ordinaire ; exactement une source est permise. Incompatible
  avec `-t`.
- `-h, -?, --help` — afficher l'aide courte de cette commande.

## EXAMPLES

- `cp notes.txt backup.txt` — copier un fichier sous un nouveau nom.
- `cp -r Projects Archive` — reproduire l'arbre `Projects` dans
  `Archive` (ou comme `Archive` s'il n'existe pas).
- `cp -v -t Backup a.txt b.txt` — copier les deux fichiers dans
  `Backup`, en signalant chaque copie.

## EXIT STATUS

- `0` — chaque copie a réussi (un saut `-n` et une question `-i`
  déclinée ne sont pas des échecs).
- `1` — un échec du système de fichiers, de la question ou de la
  sortie ; la raison est imprimée sur la sortie d'erreur.
- `2` — la ligne de commande n'a pas été comprise.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette
  BCP-47 telle que `fr-FR`).

## SEE ALSO

- `ls`
- `mv`
- `rm`
