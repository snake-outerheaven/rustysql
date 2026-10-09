CREATE TABLE pessoas (id INT PRIMARY KEY, nome TEXT NOT NULL, idade INT, nota FLOAT);
INSERT INTO pessoas VALUES (1, 'Ana', 30, 9.5), (2, 'Beto', 25, 7), (3, 'Carla', 41, NULL), (4, 'Davi', 19, 8.25);
SELECT * FROM pessoas ORDER BY idade DESC;
SELECT nome, idade + 1 FROM pessoas WHERE idade > 20 AND nota IS NOT NULL;
UPDATE pessoas SET nota = nota + 0.5 WHERE nome = 'Beto';
DELETE FROM pessoas WHERE idade < 20;
SELECT nome, nota FROM pessoas ORDER BY nota LIMIT 3;
INSERT INTO pessoas VALUES (1, 'Duplicada', 99, 0);
