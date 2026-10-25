## NAME

cp — copier des fichiers et des répertoires

## SYNOPSIS

`cp [-dfHilLnPrRsvT] [--sparse=WHEN] [--preserve=LIST] [--no-preserve=LIST] [-t dir] [--] source... dest`

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
question déclinée ignore cette copie et fait échouer l'exécution, comme
chez GNU ; une réponse illisible ne vaut jamais consentement).
Une destination qui est le fichier source lui-même — le même nom, un
autre nom de ce fichier ou un lien vers lui — est refusée comme étant
le même fichier et reste telle qu'elle était, sauf que `-l` la considère
comme déjà liée. Un lien que `-f` met à la place d'un nom existant prend
cette place en une seule étape, et il est créé avant que quoi que ce
soit ne soit remplacé, de sorte qu'un lien qui ne peut pas être créé
laisse le nom en place.

Un fichier impossible à lire, à créer ou à écrire est signalé, et les
autres sont tout de même copiés, y compris le reste de son répertoire.
`--` termine l'analyse des options : chaque argument ultérieur est un
chemin.

Un nouveau fichier est créé avec les bits de permission de sa source,
sans les bits set-ID et sticky, moins l'umask, de sorte que la copie
d'un fichier privé reste privée ; une destination qui existe déjà garde
son propre mode. Un nouveau répertoire reçoit pour finir les bits de
permission et sticky de sa source, moins l'umask ; pendant que ses
entrées y sont copiées, il reste ouvert à son propriétaire, de sorte
qu'un répertoire source en lecture seule est tout de même copié en
entier, de même que lorsque l'umask retire l'écriture au propriétaire.
On n'écrit jamais à travers un lien symbolique rencontré dans une
arborescence de destination.

Le `-a`/`--archive` de GNU et les ensembles `--preserve` autres que
`--preserve=links,xattr` sont **refusés**, et non restreints. `-a`
équivaut à `-dR --preserve=all`, et `--preserve=all` inclut les
horodatages d'un nœud, qu'aucun appel de ce système ne peut fixer —
honorer `-a` signalerait donc une préservation qui n'a pas eu lieu.
Utilisez `-dR` pour le reste.

Chaque copie emporte les attributs étendus de la source — un type de
fichier RISC OS, le type et le créateur d'un Mac — partout où la
destination peut les conserver, et garde comme trous les trous d'un
fichier creux. Les attributs qu'une destination ne peut pas conserver
sont signalés par un seul avertissement à la fin de l'exécution, et par
un enregistrement `omission` sur le flux d'information standard ; avec
`--preserve=xattr`, une telle copie échoue à la place.

Un répertoire n'est jamais copié dans lui-même : une destination à
l'intérieur de la source — quelle que soit son écriture, par un lien, un
`..` ou un autre nom du volume — est refusée avant que rien ne soit
écrit, et un répertoire déplacé dans la source pendant la copie arrête
la copie là où le parcours le rencontre.

## OPTIONS

- `-r, -R, --recursive` — copier les répertoires et leur contenu.
- `-f, --force` — quand un fichier de destination ne peut pas être
  créé, le supprimer et réessayer la copie une fois ; avec `-l`, `-s` ou
  un lien que `-P` reproduit, mettre le lien à la place d'un nom
  existant.
- `-i, --interactive` — demander avant d'écraser un fichier existant ;
  seule une réponse commençant par `y`/`Y` consent, et une question
  déclinée ignore ce fichier et fait échouer l'exécution.
- `-n, --no-clobber` — ne jamais écraser un fichier existant. Le
  dernier de `-i` et `-n` l'emporte.
- `-l, --link` — donner à la destination un second nom du nœud de la
  source au lieu de copier ses octets, de sorte que les deux noms ne
  puissent diverger à une écriture ultérieure. Un répertoire source
  exige toujours `-r`.
- `-s, --symbolic-link` — créer un lien symbolique désignant la source
  au lieu de la copier.
- `-P, --no-dereference` — reproduire chaque lien symbolique parmi les
  sources comme un lien de même cible, mot pour mot, au lieu de copier
  ce qu'il désigne (ainsi un lien relatif ou pendant survit à la copie).
- `-L, --dereference` — suivre chaque lien symbolique parmi les sources
  et copier ce qu'il désigne.
- `-H` — suivre un lien symbolique nommé comme opérande, mais reproduire
  ceux rencontrés dans une arborescence. Sans `-H`, `-L` ni `-P`, une
  copie récursive reproduit chaque lien, ceux de ses opérandes compris,
  et toute autre copie suit les liens de ses opérandes, comme celle de
  GNU ; `-l` lie ce que désigne un lien suivi.
- `--preserve=links` — deux sources désignant un même nœud obtiennent
  deux noms à la destination plutôt que deux copies, de sorte que la
  copie ne double pas silencieusement le stockage.
- `--preserve=xattr` — chaque attribut étendu doit être emporté : une
  copie dont la destination ne peut en conserver un échoue en le
  nommant. `--preserve=links,xattr` demande les deux.
- `--no-preserve=xattr` — ne copier aucun attribut étendu. `ownership`
  et `timestamps` sont aussi acceptés, et déjà non préservés.
- `--no-preserve=mode` — créer chaque nouveau fichier et chaque nouveau
  répertoire avec le mode que reçoit un élément nouveau quand rien n'en
  désigne un, moins l'umask, plutôt qu'avec celui de sa source.
  `--no-preserve=all` le fait aussi.
- `--sparse=WHEN` — la manière de copier un fichier creux : `auto`, la
  valeur par défaut, garde ses trous ; `always` laisse aussi non écrit
  chaque bloc de zéros qu'il stocke ; `never` écrit chaque octet.
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

- `0` — chaque copie a réussi (un saut `-n` n'est pas un échec).
- `1` — un fichier n'a pas pu être copié, une question `-i` a été
  déclinée, ou une question ou la sortie a échoué ; la raison de chaque
  échec est imprimée sur la sortie d'erreur. Une ligne de commande non
  comprise sort aussi avec `1`.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette
  BCP-47 telle que `fr-FR`).

## SEE ALSO

- `ls`
- `mv`
- `rm`
