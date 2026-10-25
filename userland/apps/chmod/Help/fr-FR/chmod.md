## NAME

chmod — modifier les bits de mode d'un fichier

## SYNOPSIS

`chmod [-cfRv] [--] MODE file...`

## DESCRIPTION

Change les bits de permission de chaque opérande fichier en `MODE`,
dans l'ordre. `MODE` est soit une valeur octale absolue (`644`,
`0755`, …) qui remplace entièrement les bits de permission, soit une
liste de clauses symboliques séparées par des virgules
`[ugoa]*([-+=]([rwxXst]*|[ugo]))+` ou `[-+=][0-7]+` (`g+w`, `o-rx`,
`a=rx`, `u+s`, `g=u`, `=644`) qui transforment les bits actuels du
fichier. Le `X` symbolique n'accorde l'exécution qu'à un répertoire ou
à un fichier portant déjà un bit d'exécution, `g=u` donne au groupe les
bits du propriétaire, `s` positionne les bits set-ID du propriétaire et
du groupe, et `t` positionne le bit sticky. Une clause qui ne nomme
personne (`+x`, `=r`) modifie tous les champs, sauf les bits que
contient l'umask.

Sur un répertoire, les bits set-user-ID et set-group-ID restent tels
quels à moins que le mode ne les nomme : avec `s` (`g-s`), avec de
l'octal après un opérateur (`=755`), ou avec une valeur octale de cinq
chiffres ou plus (`00755`). C'est le bit set-group-ID d'un répertoire
qui donne son groupe à ses nouvelles entrées, si bien que `chmod 755`
le conserve.

Seul le propriétaire d'un fichier peut changer son mode ; le noyau
refuse quiconque d'autre, et détenir une capability n'accorde aucun
passe-droit. Pour un lien symbolique nommé comme opérande, c'est le
fichier qu'il désigne qui est modifié. Avec `-R`, un opérande répertoire
est modifié puis son contenu l'est récursivement ; un lien symbolique
rencontré dans l'arborescence est laissé tel quel, tout comme ce qu'il
désigne. Un fichier impossible à atteindre ou à modifier est signalé, et
les autres sont tout de même modifiés. `--` termine l'analyse des
options : chaque argument ultérieur est un opérande. Pour un mode
commençant par `-`, écrivez-le sans le tiret (`a-w`) ou terminez d'abord
les options (`chmod -- -w file`).

## OPTIONS

- `-R, --recursive` — modifier fichiers et répertoires
  récursivement.
- `-c, --changes` — ne signaler que les fichiers dont le mode a
  réellement changé.
- `-v, --verbose` — signaler chaque fichier traité, y compris celui dont
  la modification a été refusée ou qui n'a pas pu être atteint.
- `-f, --silent, --quiet` — supprimer la plupart des messages
  d'erreur ; l'exécution échoue quand même et le code de sortie le
  signale.
- `--preserve-root` — avec `-R`, refuser de modifier `/`, quel que soit
  le chemin qui y mène.
- `--no-preserve-root` — traiter `/` comme n'importe quel autre
  répertoire (le défaut).
- `-h, -?, --help` — afficher l'aide courte de cette commande.

## EXAMPLES

- `chmod 644 notes.txt` — lecture/écriture pour le propriétaire,
  lecture seule pour les autres.
- `chmod g+w shared.txt` — ajouter l'écriture du groupe aux bits
  actuels.
- `chmod -R a=rx Docs` — rendre l'arbre `Docs` lisible et
  traversable par tous.
- `chmod +x build.sh` — rendre un script exécutable par tous ceux que
  l'umask autorise.

## EXIT STATUS

- `0` — chaque changement de mode a réussi.
- `1` — un fichier n'a pas pu être atteint ou modifié, ou la sortie a
  échoué ; chaque raison est imprimée sur la sortie d'erreur (supprimée
  sous `-f`). Une ligne de commande non comprise, ou dont l'opérande de
  mode n'était ni octal ni symbolique, sort aussi avec `1`.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette
  BCP-47 telle que `fr-FR`).

## SEE ALSO

- `ls`
- `mkdir`
- `rm`
