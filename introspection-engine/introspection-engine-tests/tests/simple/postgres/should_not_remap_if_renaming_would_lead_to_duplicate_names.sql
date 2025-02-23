-- tags=postgres
-- exclude=cockroachdb

CREATE TABLE nodes(id serial primary key);
CREATE TABLE _nodes(
    node_a int NOT NULL,
    node_b int NOT NULL,
    CONSTRAINT _nodes_node_a_fkey FOREIGN KEY(node_a) REFERENCES nodes(id) ON DELETE CASCADE ON UPDATE CASCADE,
    CONSTRAINT _nodes_node_b_fkey FOREIGN KEY(node_b) REFERENCES nodes(id) ON DELETE CASCADE ON UPDATE CASCADE
);


/*
generator client {
  provider = "prisma-client-js"
}

datasource db {
  provider = "postgresql"
  url      = "env(TEST_DATABASE_URL)"
}

/// The underlying table does not contain a valid unique identifier and can therefore currently not be handled by the Prisma Client.
model prisma_tests__nodes {
  node_a                    Int
  node_b                    Int
  nodes_nodes_node_aTonodes prisma_tests_nodes @relation("nodes_node_aTonodes", fields: [node_a], references: [id], onDelete: Cascade)
  nodes_nodes_node_bTonodes prisma_tests_nodes @relation("nodes_node_bTonodes", fields: [node_b], references: [id], onDelete: Cascade)

  @@map("_nodes")
  @@ignore
}

model prisma_tests_nodes {
  id                        Int                   @id @default(autoincrement())
  nodes_nodes_node_aTonodes prisma_tests__nodes[] @relation("nodes_node_aTonodes") @ignore
  nodes_nodes_node_bTonodes prisma_tests__nodes[] @relation("nodes_node_bTonodes") @ignore

  @@map("nodes")
}
*/
