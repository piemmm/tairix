## NAME

rm — supprimer des fichiers et des répertoires

## SYNOPSIS

`rm [-dfiIrRv] [--] file...`

## DESCRIPTION

Supprime chaque opérande fichier, dans l'ordre. Un opérande qui n'est
pas un répertoire est délié — un lien symbolique est lui-même supprimé,
jamais ce qu'il désigne ; un opérande répertoire n'est supprimé qu'avec
`-r` (qui supprime son contenu en profondeur d'abord, puis le répertoire
lui-même) ou, s'il est vide, avec `-d`. Dans les deux cas, un opérande
dont le dernier composant est `.` ou `..` est refusé, comme l'exige
POSIX.

Avec `-f`, un opérande qui n'existe pas est ignoré silencieusement et
aucune question n'est jamais posée. `-i` demande sur la sortie
d'erreur avant chaque suppression, en nommant ce qu'il supprimerait
(`remove regular empty file 'a'?`, `remove symbolic link 'l'?`), et
avant de descendre dans un répertoire qui contient quelque chose ; `-I`
demande une seule fois avant de supprimer plus de trois opérandes ou
avant une suppression récursive. Une question déclinée conserve l'objet
(ou ignore toute l'exécution, pour `-I`) sans erreur ; une réponse
illisible ne vaut jamais consentement. Le dernier de `-f`, `-i` et `-I`
l'emporte.

Sans `-f`, lorsque l'entrée standard est un terminal, un fichier sur
lequel l'appelant n'a pas le droit d'écrire fait l'objet d'une question
avant d'être supprimé (`remove write-protected regular file 'x'?`) et
n'est supprimé que sur une réponse commençant par `y` ou `Y` ; un lien
symbolique ne fait jamais l'objet d'une question. Sous `-i`, la question
nomme aussi la protection.

Un chemin impossible à examiner, à lire ou à supprimer est signalé, et
les autres sont tout de même supprimés ; un répertoire qui contient
encore quelque chose est conservé sans un mot. Une suppression récursive
de `/`, quelle qu'en soit l'écriture, est refusée sous
`--preserve-root`, le comportement par défaut. `--` termine l'analyse
des options : chaque argument ultérieur est un chemin.

## OPTIONS

- `-r, -R, --recursive` — supprimer les répertoires et leur contenu.
- `-f, --force` — ignorer les opérandes qui n'existent pas ; ne
  jamais demander.
- `-d, --dir` — supprimer les répertoires vides.
- `-i, --interactive` — demander avant chaque suppression ; seule une
  réponse commençant par `y`/`Y` consent.
- `-I` — demander une seule fois avant de supprimer plus de trois
  opérandes, ou avant une suppression récursive.
- `-v, --verbose` — signaler chaque suppression sous la forme
  `removed 'file'`.
- `--preserve-root` — refuser de supprimer `/` récursivement (le
  défaut).
- `--no-preserve-root` — autoriser la suppression de `/`. L'option n'est
  acceptée qu'écrite en toutes lettres.
- `-h, -?, --help` — afficher l'aide courte de cette commande.

## EXAMPLES

- `rm notes.txt` — supprimer un fichier.
- `rm -r Scratch` — supprimer l'arbre `Scratch` et tout son contenu.
- `rm -I a b c d` — demander une fois, puis supprimer les quatre
  fichiers sur un `y`.

## EXIT STATUS

- `0` — chaque suppression a réussi (une question déclinée et un saut
  `-f` ne sont pas des échecs).
- `1` — un chemin n'a pas pu être supprimé, ou une question ou la sortie
  a échoué ; chaque raison est imprimée sur la sortie d'erreur. Une
  ligne de commande non comprise sort aussi avec `1`.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette
  BCP-47 telle que `fr-FR`).

## SEE ALSO

- `cp`
- `ls`
- `mv`
