// The quarb-neo4j test fixture: an org chart with a self-referential
// REPORTS_TO relationship (one multi-label node and one plain
// Person), a FRIEND graph carrying relationship properties (present
// but unqueried in v1 — the visible gap), and a two-level bill of
// materials. Load into an empty database:
//
//   docker exec -i quarb-neo4j cypher-shell < fixture.cypher
//
// Every node carries an `id` property so `?key=id` naming works.
CREATE (alice:Person:Employee {id: 1, name: 'Alice', title: 'CEO'}),
       (bob:Person:Employee   {id: 2, name: 'Bob',   title: 'CTO'}),
       (carol:Person:Employee {id: 3, name: 'Carol', title: 'Engineer'}),
       (dan:Person:Employee   {id: 4, name: 'Dan',   title: 'Engineer'}),
       (eve:Person            {id: 5, name: 'Eve'}),
       (bob)-[:REPORTS_TO]->(alice),
       (carol)-[:REPORTS_TO]->(bob),
       (dan)-[:REPORTS_TO]->(bob),
       (bob)-[:FRIEND {since: 2015}]->(carol),
       (carol)-[:FRIEND {since: 2020}]->(eve),
       (widget:Part {id: 100, name: 'Widget'}),
       (gear:Part   {id: 101, name: 'Gear'}),
       (tooth:Part  {id: 102, name: 'Tooth'}),
       (widget)-[:CONTAINS {qty: 4}]->(gear),
       (gear)-[:CONTAINS {qty: 12}]->(tooth);
