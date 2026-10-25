## NAME

chown — modifier le propriétaire et le groupe d'un fichier

## SYNOPSIS

`chown [OPTION]... [OWNER][:[GROUP]] file...`

`chown [OPTION]... --reference=RFILE file...`

## DESCRIPTION

Change l'utilisateur et le groupe propriétaires de chaque opérande
fichier, dans l'ordre. `OWNER` change l'utilisateur et garde le groupe,
`OWNER:GROUP` change les deux, et `:GROUP` ne change que le groupe.
`OWNER:` donne en outre au fichier le groupe de connexion du
propriétaire. Un opérande vide, ou `:` seul, ne change ni l'un ni
l'autre. L'ancienne écriture `OWNER.GROUP` fonctionne avec un
avertissement, sauf si l'opérande entier désigne un compte. Avec
`--reference`, chaque fichier prend le propriétaire et le groupe de ce
que désigne `RFILE`.

`OWNER` et `GROUP` sont des noms de compte et de groupe, ou des
nombres : un nom est essayé d'abord, et un nombre n'est retenu que
lorsque rien ne porte ce nom. Un `+` initial (`+1000`) désigne toujours
un nombre.

Donner un fichier à un autre compte, ou à un groupe dont vous n'êtes pas
membre, exige la capacité `CAP_FS_CHOWN` ; sans elle, vous pouvez
changer le groupe d'un fichier qui vous appartient pour l'un de vos
propres groupes. Un changement efface le bit set-user-ID d'un fichier,
et son bit set-group-ID lorsque son groupe peut l'exécuter ; un
répertoire garde les deux.

Pour un lien symbolique nommé comme opérande, c'est le fichier qu'il
désigne qui est modifié, ou avec `-h` le lien lui-même. Avec `-R`, un
opérande répertoire et tout ce qu'il contient sont modifiés, chaque
répertoire après son contenu. Un lien symbolique rencontré dans
l'arborescence est lui-même modifié, et ce qu'il désigne n'est jamais
touché ; avec `--dereference`, le lien est lui aussi laissé tel quel.
Avec `-R`, un lien symbolique nommé comme opérande est lui-même modifié
et n'est pas suivi : `--dereference` modifie plutôt ce qu'il désigne, et
`-H` parcourt un répertoire qu'il désigne. Un fichier impossible à
atteindre ou à modifier est signalé, et les autres sont tout de même
modifiés. Un rapport nomme chaque propriétaire et chaque groupe par son
nom, ou par son numéro quand aucun compte ni aucun groupe ne le porte.
`--` termine l'analyse des options : chaque argument ultérieur est un
opérande.

## OPTIONS

- `-c, --changes` — ne signaler que les fichiers dont le propriétaire ou
  le groupe a réellement changé.
- `-f, --silent, --quiet` — supprimer la plupart des messages d'erreur ;
  l'exécution échoue quand même et le code de sortie le signale.
- `-v, --verbose` — signaler chaque fichier traité, y compris celui dont
  la modification a été refusée ou qui n'a pas pu être atteint.
- `--dereference` — modifier ce que désigne un lien symbolique plutôt
  que le lien ; sans `-R`, c'est le comportement par défaut.
- `-h, --no-dereference` — modifier un lien symbolique lui-même plutôt
  que ce qu'il désigne.
- `--preserve-root` — avec `-R`, refuser de modifier `/`, quel que soit
  le chemin qui y mène.
- `--no-preserve-root` — traiter `/` comme n'importe quel autre
  répertoire (le défaut).
- `--from=CURRENT_OWNER:CURRENT_GROUP` — ne modifier un fichier que si
  son propriétaire et son groupe actuels sont ceux-ci ; omettre l'un des
  deux revient à accepter n'importe quelle valeur.
- `--reference=RFILE` — donner à chaque fichier le propriétaire et le
  groupe de `RFILE` au lieu de les nommer.
- `-R, --recursive` — modifier fichiers et répertoires récursivement.
- `-H` — avec `-R`, parcourir le répertoire auquel mène un lien
  symbolique nommé comme opérande.
- `-L` — avec `-R`, suivre chaque lien symbolique menant à un
  répertoire ; refusé, car une modification récursive ne suit jamais un
  lien dans l'arborescence.
- `-P` — avec `-R`, ne suivre aucun lien symbolique (le défaut). Le
  dernier donné de `-H`, `-L` et `-P` l'emporte.
- `-?, --help` — afficher l'aide courte de cette commande.
- `--version` — imprimer le nom et la version de l'outil.

## EXAMPLES

- `chown alice notes.txt` — donner `notes.txt` à `alice`, en gardant son
  groupe.
- `chown alice:staff notes.txt` — le donner à `alice` et au groupe
  `staff`.
- `chown :staff shared.txt` — ne changer que le groupe.
- `chown -R alice Projects` — donner l'arbre `Projects` à `alice`.
- `chown -R --from=bob alice Shared` — ne donner à `alice` que ce que
  `bob` possède dans `Shared`.
- `chown --reference=template.txt notes.txt` — donner à `notes.txt` le
  propriétaire et le groupe de `template.txt`.

## EXIT STATUS

- `0` — chaque changement a réussi.
- `1` — un fichier n'a pas pu être atteint ou modifié, ou la sortie a
  échoué ; chaque raison est imprimée sur la sortie d'erreur (supprimée
  sous `-f`). Une ligne de commande non comprise, ou un propriétaire, un
  groupe ou un fichier de référence impossible à lire, sort aussi avec
  `1` et ne change rien.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette
  BCP-47 telle que `fr-FR`).

## SEE ALSO

- `chmod`
- `ls`
- `stat`
